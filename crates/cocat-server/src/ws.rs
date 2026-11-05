//! The websocket route: authenticate the cookie and check the role before the handshake,
//! then bridge the socket to the room's actor thread.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

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
use tokio::time::timeout;

use crate::AppState;
use crate::room::{ClientId, Inbox, RoomError, RoomGone, RoomHandle, outbox};
use crate::{auth, db};

pub const INTERNAL_ERROR: u16 = 1011;

/// A client that stopped reading leaves the socket's send buffer full forever; aborting the
/// pending write after this long drops the socket, which closes TCP and lets the room unload.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

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
    let mut writer = tokio::spawn(async move {
        if let Err(error) = write_until_closed(&mut sink, inbox).await {
            tracing::debug!(%error, "the socket stopped accepting frames");
        }
    });

    let mut writer_finished = false;
    loop {
        tokio::select! {
            finished = &mut writer => {
                writer_finished = true;
                if let Err(error) = finished {
                    tracing::warn!(%error, "the writer task failed");
                }
                break;
            }
            frame = stream.next() => match frame {
                Some(Ok(Message::Binary(bytes))) => room.frame(client, bytes),
                Some(Ok(Message::Close(_))) | None => break,
                Some(Err(error)) => {
                    tracing::debug!(%error, "the socket failed");
                    break;
                }
                Some(Ok(_)) => {}
            },
        }
    }

    // The writer is the only one that knows when the socket stopped working, and it must
    // not outlive this bridge: a client that neither reads nor closes would otherwise keep
    // the room loaded forever.
    drop(stream);
    room.detach(client).await;
    if !writer_finished {
        writer.abort();
        let _ = writer.await;
    }
}

#[derive(Debug, Error)]
enum WriterError {
    #[error(transparent)]
    Socket(#[from] axum::Error),
    #[error("the client accepted no frame within {0:?}")]
    Timeout(Duration),
}

/// Writes the room's frames until it closes the client or the socket fails, then sends the
/// close frame. Every write is bounded: a client that stopped reading leaves `send` pending
/// forever, and dropping the sink when the write times out closes TCP, which is what frees
/// the room (the close code is lost, but that client is not reading it anyway).
async fn write_until_closed(
    sink: &mut SplitSink<WebSocket, Message>,
    mut inbox: Inbox,
) -> Result<(), WriterError> {
    let reason = loop {
        let frame = tokio::select! {
            biased;
            reason = &mut inbox.close => break reason.ok(),
            frame = inbox.frames.recv() => match frame {
                Some(frame) => frame,
                None => break None,
            },
        };
        write(sink, Message::Binary(frame)).await?;
    };
    let frame = reason.map(|reason| CloseFrame {
        code: reason.code(),
        reason: reason.reason().into(),
    });
    write(sink, Message::Close(frame)).await
}

async fn write(
    sink: &mut SplitSink<WebSocket, Message>,
    message: Message,
) -> Result<(), WriterError> {
    match timeout(WRITE_TIMEOUT, sink.send(message)).await {
        Ok(sent) => Ok(sent?),
        Err(_) => Err(WriterError::Timeout(WRITE_TIMEOUT)),
    }
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
