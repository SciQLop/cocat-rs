//! The websocket route: authenticate the cookie and check the role before the handshake,
//! then bridge the socket to the room's actor thread.

use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use cocat_core::role::Role;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use thiserror::Error;

use crate::AppState;
use crate::room::{ClientId, Inbox, RoomError, RoomGone, RoomHandle, outbox};
use crate::{auth, db};

pub const INTERNAL_ERROR: u16 = 1011;

const ATTACH_TRIES: usize = 3;

static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

/// One frame per broadcast is small; Nagle's delayed ACK would delay it to the next tick.
pub fn disable_nagle(stream: &mut tokio::net::TcpStream) {
    if let Err(error) = stream.set_nodelay(true) {
        tracing::warn!(%error, "cannot disable Nagle on an accepted socket");
    }
}

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
    let (room, inbox) = match attach(&state, &room_id, client, role).await {
        Ok(attached) => attached,
        Err(error) => {
            tracing::warn!(%error, room = %room_id, "cannot attach the socket");
            return close(socket, INTERNAL_ERROR, "cannot attach to the room").await;
        }
    };

    let (mut sink, mut stream) = socket.split();
    let writer = tokio::spawn(async move {
        if let Err(error) = write_until_closed(&mut sink, inbox).await {
            tracing::debug!(%error, "the socket stopped accepting frames");
        }
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

/// Writes the room's frames until it closes the client or the socket fails, then sends the
/// close frame. A reason that arrives while a write is blocked reaches the client as soon
/// as it reads again: aborting the pending write could truncate a frame, and the socket
/// buffers already hold frames that cannot be recalled.
async fn write_until_closed(
    sink: &mut SplitSink<WebSocket, Message>,
    mut inbox: Inbox,
) -> Result<(), axum::Error> {
    let reason = loop {
        let frame = tokio::select! {
            biased;
            reason = &mut inbox.close => break reason.ok(),
            frame = inbox.frames.recv() => match frame {
                Some(frame) => frame,
                None => break None,
            },
        };
        sink.send(Message::Binary(frame)).await?;
    };
    let frame = reason.map(|reason| CloseFrame {
        code: reason.code(),
        reason: reason.reason().into(),
    });
    sink.send(Message::Close(frame)).await
}

async fn attach(
    state: &AppState,
    room_id: &str,
    client: ClientId,
    role: Role,
) -> Result<(RoomHandle, Inbox), AttachError> {
    for _ in 0..ATTACH_TRIES {
        let rooms = state.rooms.clone();
        let id = room_id.to_owned();
        let room = tokio::task::spawn_blocking(move || rooms.get_or_open(&id))
            .await?
            .map_err(AttachError::Room)?;
        let (out, inbox) = outbox();
        match room.attach(client, role, out) {
            Ok(()) => return Ok((room, inbox)),
            Err(RoomGone) => continue,
        }
    }
    Err(AttachError::Gone(RoomGone))
}

fn next_client() -> ClientId {
    NEXT_CLIENT.fetch_add(1, Ordering::Relaxed)
}
