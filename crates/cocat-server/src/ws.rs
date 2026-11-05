//! The websocket route: authenticate the cookie and check the role before the handshake,
//! then bridge the socket to the room's actor thread.

use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use bytes::Bytes;
use cocat_core::role::Role;
use futures_util::{SinkExt, StreamExt};
use thiserror::Error;
use tokio::sync::mpsc;

use crate::AppState;
use crate::room::{ClientId, RoomError, RoomGone, RoomHandle};
use crate::{auth, db};

pub const INTERNAL_ERROR: u16 = 1011;

const ATTACH_TRIES: usize = 3;

static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Error)]
enum AttachError {
    #[error(transparent)]
    Room(#[from] RoomError),
    #[error("the room task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error(transparent)]
    Gone(#[from] RoomGone),
}

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
    let role = match lobby_role(&state, &room_id, &headers).await {
        Some(role) => role,
        None => return StatusCode::FORBIDDEN.into_response(),
    };
    upgrade.on_upgrade(move |socket| bridge(socket, state, room_id, role))
}

async fn lobby_role(state: &AppState, room_id: &str, headers: &HeaderMap) -> Option<Role> {
    let user = auth::authenticate(&state.pool, headers, &state.secret)
        .await
        .ok()?;
    db::role_in(&state.pool, room_id, user).await.ok()?
}

async fn close(mut socket: WebSocket, code: u16, reason: &'static str) {
    let frame = CloseFrame {
        code,
        reason: reason.into(),
    };
    let _ = socket.send(Message::Close(Some(frame))).await;
}

async fn bridge(socket: WebSocket, state: AppState, room_id: String, role: Role) {
    let client = next_client();
    let (out, mut frames) = mpsc::unbounded_channel();
    let room = match attach(&state, &room_id, client, role, out).await {
        Ok(room) => room,
        Err(error) => {
            tracing::warn!(%error, room = %room_id, "cannot attach the socket");
            return close(socket, INTERNAL_ERROR, "cannot attach to the room").await;
        }
    };

    let (mut sink, mut stream) = socket.split();
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

    room.detach(client).await;
    let _ = writer.await;
}

async fn attach(
    state: &AppState,
    room_id: &str,
    client: ClientId,
    role: Role,
    out: mpsc::UnboundedSender<Bytes>,
) -> Result<RoomHandle, AttachError> {
    for _ in 0..ATTACH_TRIES {
        let rooms = state.rooms.clone();
        let id = room_id.to_owned();
        let room = tokio::task::spawn_blocking(move || rooms.get_or_open(&id))
            .await?
            .map_err(AttachError::Room)?;
        match room.attach(client, role, out.clone()) {
            Ok(()) => return Ok(room),
            Err(RoomGone) => continue,
        }
    }
    Err(AttachError::Gone(RoomGone))
}

fn next_client() -> ClientId {
    NEXT_CLIENT.fetch_add(1, Ordering::Relaxed)
}
