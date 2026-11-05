//! The websocket route: authenticate the cookie, check the role, then bridge the socket
//! to the room's actor thread.

use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::{get, post};
use cocat_core::role::Role;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;

use crate::AppState;
use crate::room::{ClientId, RoomHandle};
use crate::{auth, db};

pub const POLICY_VIOLATION: u16 = 1008;
pub const INTERNAL_ERROR: u16 = 1011;

static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/auth/jwt/login", post(auth::login_handler))
        .route("/room/{room_id}", get(room_socket))
        .with_state(state)
}

async fn room_socket(
    upgrade: WebSocketUpgrade,
    Path(room_id): Path<String>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    upgrade.on_upgrade(move |socket| serve(socket, room_id, headers, state))
}

async fn serve(socket: WebSocket, room_id: String, headers: HeaderMap, state: AppState) {
    let Ok(user) = auth::authenticate(&state.pool, &headers, &state.secret).await else {
        return close(socket, POLICY_VIOLATION, "not logged in").await;
    };
    let role = match db::role_in(&state.pool, &room_id, user).await {
        Ok(Some(role)) => role,
        Ok(None) | Err(_) => return close(socket, POLICY_VIOLATION, "not a member").await,
    };
    let room = match state.rooms.get_or_open(&room_id) {
        Ok(room) => room,
        Err(error) => {
            tracing::warn!(%error, room = %room_id, "cannot open the room");
            return close(socket, INTERNAL_ERROR, "cannot open the room").await;
        }
    };
    bridge(socket, room, role).await;
}

async fn close(mut socket: WebSocket, code: u16, reason: &'static str) {
    let frame = CloseFrame {
        code,
        reason: reason.into(),
    };
    let _ = socket.send(Message::Close(Some(frame))).await;
}

async fn bridge(socket: WebSocket, room: RoomHandle, role: Role) {
    let client = next_client();
    let (out, mut frames) = mpsc::unbounded_channel();
    let (mut sink, mut stream) = socket.split();
    room.attach(client, role, out);
    let writer = tokio::spawn(async move {
        while let Some(frame) = frames.recv().await {
            if sink.send(Message::Binary(frame)).await.is_err() {
                return;
            }
        }
        let _ = sink.send(Message::Close(None)).await;
    });

    while let Some(Ok(frame)) = stream.next().await {
        match frame {
            Message::Binary(bytes) => room.frame(client, bytes),
            Message::Close(_) => break,
            _ => {}
        }
    }

    room.detach(client);
    let _ = writer.await;
}

fn next_client() -> ClientId {
    NEXT_CLIENT.fetch_add(1, Ordering::Relaxed)
}
