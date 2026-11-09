//! The websocket route: authenticate the cookie or bearer token and check the role before the handshake,
//! then bridge the socket to the room's actor thread.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use cocat_core::role::Role;
use cocat_core::{ROLE_HEADER, SUBPROTOCOL};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use thiserror::Error;
use tokio::sync::{broadcast, oneshot};
use tokio::time::timeout;

use crate::db::User;
use crate::room::{ClientId, CloseReason, Inbox, RoomError, RoomGone, RoomHandle, outbox};
use crate::{AppState, auth, db, ui};

pub const INTERNAL_ERROR: u16 = 1011;

/// A client that stopped reading leaves the socket's send buffer full forever; aborting the
/// pending write after this long drops the socket, which closes TCP and lets the room unload.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

const ATTACH_TRIES: usize = 3;

/// How often every open socket's role is read again. The API and admin pages close a
/// socket as soon as its role changes; the command line runs in its own process and only
/// reaches the database, so its changes land at the next check.
pub const ROLE_CHECK_PERIOD: Duration = Duration::from_secs(30);

/// The largest frame or message a socket accepts: uvicorn's default `ws_max_size`, so a
/// client cannot make the server buffer more than this. A bigger one is closed with 1009.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

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
    let crate::openapi::Api { router, document } =
        crate::openapi::api(&state.login_limit, &state.links);
    Router::new()
        .merge(router)
        .route("/", get(ui::landing))
        .nest("/ui/", ui::router(&state))
        .route("/room/{room_id}", get(room_socket))
        .merge(crate::openapi::router(&state.links, document))
        .layer(middleware::from_fn(ui::same_site))
        .layer(middleware::from_fn(crate::telemetry::track_http))
        .with_state(state)
}

async fn room_socket(
    upgrade: WebSocketUpgrade,
    Path(room_id): Path<String>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    let upgrade = upgrade.protocols([SUBPROTOCOL]);
    if upgrade.selected_protocol().is_none() {
        return old_client();
    }
    let (user, role) = match lobby_role(&state, &room_id, &headers).await {
        Ok(admitted) => admitted,
        Err(refusal) => return refusal.into_response(),
    };
    let Some(session) = auth::request_token_hash(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let mut response = upgrade
        .max_message_size(MAX_MESSAGE_BYTES)
        .max_frame_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| bridge(socket, state, room_id, user, role, session));
    response.headers_mut().insert(
        HeaderName::from_static(ROLE_HEADER),
        HeaderValue::from_static(role.as_str()),
    );
    response
}

/// Python cocat and the old SciQLop plugin offer no subprotocol and would write the first
/// layout into a v2 room. 426 rather than 403, which clients read as "log in again".
fn old_client() -> Response {
    (
        StatusCode::UPGRADE_REQUIRED,
        [(header::UPGRADE, "websocket")],
        format!("this server needs a client that speaks {SUBPROTOCOL}; update cocat"),
    )
        .into_response()
}

/// The account and its role, or the refusal a client can act on: 401 means the session is
/// gone (sign in again), 403 that the account has no place in this project.
async fn lobby_role(
    state: &AppState,
    room_id: &str,
    headers: &HeaderMap,
) -> Result<(User, Role), StatusCode> {
    let id = match auth::authenticate(&state.pool, headers).await {
        Ok(id) => id,
        Err(auth::AuthError::Db(error)) => return Err(server_failed(&error)),
        Err(_) => return Err(StatusCode::UNAUTHORIZED),
    };
    let role = db::role_in(&state.pool, room_id, id)
        .await
        .map_err(|error| server_failed(&error))?
        .ok_or(StatusCode::FORBIDDEN)?;
    let user = db::user_by_id(&state.pool, id)
        .await
        .map_err(|error| server_failed(&error))?
        .ok_or(StatusCode::UNAUTHORIZED)?;
    Ok((user, role))
}

fn server_failed(error: &db::DbError) -> StatusCode {
    tracing::warn!(%error, "cannot read who may open the room");
    StatusCode::INTERNAL_SERVER_ERROR
}

async fn close(mut socket: WebSocket, code: u16, reason: &'static str) {
    let frame = CloseFrame {
        code,
        reason: reason.into(),
    };
    let _ = socket.send(Message::Close(Some(frame))).await;
}

async fn bridge(
    socket: WebSocket,
    state: AppState,
    room_id: String,
    user: User,
    role: Role,
    session: String,
) {
    let mut ended_sessions = state.rooms.ended_sessions();
    if let Some(hook) = state.before_attach.clone() {
        hook().await;
    }
    let client = next_client();
    let (room, inbox) = match attach(&state, &room_id, client, &user, role).await {
        Ok(attached) => attached,
        Err(AttachError::Room(RoomError::OverBudget {
            room_id,
            needed,
            budget,
        })) => {
            tracing::warn!(
                room = %room_id,
                needed,
                budget,
                "refusing a socket: the room does not fit in the memory budget"
            );
            let refusal = CloseReason::OverBudget;
            crate::telemetry::ws_closed(refusal.label());
            return close(socket, refusal.code(), refusal.reason()).await;
        }
        Err(error) => {
            tracing::warn!(%error, room = %room_id, "cannot attach the socket");
            return close(socket, INTERNAL_ERROR, "cannot attach to the room").await;
        }
    };

    let close_now = recheck_after_attach(&state, &room_id, user.id, role, &session).await;

    crate::telemetry::ws_connected(1);
    let (mut sink, mut stream) = socket.split();
    let (close_request, close_requested) = oneshot::channel();
    let mut writer = tokio::spawn(async move {
        if let Err(error) = write_until_closed(&mut sink, inbox, close_now, close_requested).await {
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
                Some(Ok(Message::Binary(bytes))) => {
                    crate::telemetry::ws_frame(crate::telemetry::Direction::In, bytes.len());
                    room.frame(client, bytes)
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Err(error)) => {
                    if is_too_large(&error) {
                        let _ = close_request.send(CloseReason::TooLarge);
                    }
                    tracing::debug!(%error, "the socket failed");
                    break;
                }
                Some(Ok(_)) => {}
            },
            ended = ended_sessions.recv() => {
                if this_session_ended(&state, &session, ended).await {
                    let _ = close_request.send(CloseReason::SessionEnded);
                    break;
                }
            }
        }
    }

    // The writer is the only one that knows when the socket stopped working, and it must
    // not outlive this bridge: a client that neither reads nor closes would otherwise keep
    // the room loaded forever. It gets one write timeout to send the close frame it owes the
    // client for an oversized message before the bridge gives up on it.
    drop(stream);
    room.detach(client).await;
    crate::telemetry::ws_connected(-1);
    if !writer_finished && timeout(WRITE_TIMEOUT, &mut writer).await.is_err() {
        writer.abort();
        let _ = writer.await;
    }
}

/// The room only knows the role the handshake granted; a revocation can land while the
/// socket is being attached. A role that cannot be re-read is not trusted either, but that is
/// the server failing, so the client retries with a backoff instead of reconnecting at once.
async fn recheck_after_attach(
    state: &AppState,
    room_id: &str,
    user: uuid::Uuid,
    role: Role,
    session: &str,
) -> Option<CloseReason> {
    if !session_alive(state, session).await {
        return Some(CloseReason::SessionEnded);
    }
    match db::role_in(&state.pool, room_id, user).await {
        Ok(current) if current == Some(role) => None,
        Ok(_) => Some(CloseReason::AccessChanged),
        Err(error) => {
            tracing::warn!(%error, room = %room_id, "cannot re-read the role after the attach");
            Some(CloseReason::Failed)
        }
    }
}

/// Whether a revocation the socket heard of is its own. A socket that fell behind the
/// broadcast reads its session again rather than guess.
async fn this_session_ended(
    state: &AppState,
    session: &str,
    ended: Result<String, broadcast::error::RecvError>,
) -> bool {
    match ended {
        Ok(hash) => hash == session,
        Err(broadcast::error::RecvError::Lagged(_)) => !session_alive(state, session).await,
        // The manager in `state` holds the sender, so the channel cannot close under us.
        Err(broadcast::error::RecvError::Closed) => false,
    }
}

/// Whether the session the socket was opened with still signs its holder in. One that cannot
/// be read is not trusted.
async fn session_alive(state: &AppState, session: &str) -> bool {
    match db::session_user(&state.pool, session).await {
        Ok(holder) => holder.is_some_and(|holder| holder.user.is_active),
        Err(error) => {
            tracing::warn!(%error, "cannot re-read a socket's session");
            false
        }
    }
}

/// Whether the socket refused a message for its size, which tungstenite reports before the
/// bridge sees a frame. Everything else that breaks a socket is just dropped.
fn is_too_large(error: &axum::Error) -> bool {
    use std::error::Error as _;

    use tungstenite::Error;

    error
        .source()
        .and_then(|source| source.downcast_ref::<Error>())
        .is_some_and(|error| matches!(error, Error::Capacity(_)))
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
    close_now: Option<CloseReason>,
    close_requested: oneshot::Receiver<CloseReason>,
) -> Result<(), WriterError> {
    let reason = match close_now {
        Some(reason) => Some(reason),
        None => drain_until_close(sink, &mut inbox, close_requested).await?,
    };
    if let Some(reason) = reason {
        crate::telemetry::ws_closed(reason.label());
    }
    let frame = reason.map(|reason| CloseFrame {
        code: reason.code(),
        reason: reason.reason().into(),
    });
    write(sink, Message::Close(frame)).await
}

/// Writes queued frames until the room closes the client, then reports why.
async fn drain_until_close(
    sink: &mut SplitSink<WebSocket, Message>,
    inbox: &mut Inbox,
    mut close_requested: oneshot::Receiver<CloseReason>,
) -> Result<Option<CloseReason>, WriterError> {
    loop {
        let frame = tokio::select! {
            biased;
            // The bridge asks before it detaches, and the detach ends the inbox: its request
            // must win, or the client gets a close frame without its reason.
            reason = &mut close_requested => return Ok(reason.ok()),
            reason = &mut inbox.close => return Ok(reason.ok()),
            frame = inbox.frames.recv() => match frame {
                Some(frame) => frame,
                None => return Ok(None),
            },
        };
        crate::telemetry::ws_frame(crate::telemetry::Direction::Out, frame.len());
        write(sink, Message::Binary(frame)).await?;
    }
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
    user: &User,
    role: Role,
) -> Result<(RoomHandle, Inbox), AttachError> {
    let allowance = state.rooms.allowance(&state.pool, room_id).await?;
    for _ in 0..ATTACH_TRIES {
        let rooms = state.rooms.clone();
        let id = room_id.to_owned();
        let room = tokio::task::spawn_blocking(move || rooms.get_or_open_within(&id, allowance))
            .await?
            .map_err(AttachError::Room)?;
        let (out, inbox) = outbox();
        match room.attach(client, user.id, user.email.clone(), role, out) {
            Ok(()) => return Ok((room, inbox)),
            Err(RoomGone) => continue,
        }
    }
    Err(AttachError::Gone(RoomGone))
}

pub fn spawn_role_checks(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(ROLE_CHECK_PERIOD);
        loop {
            ticker.tick().await;
            check_roles(&state).await;
        }
    })
}

/// Closes every socket whose account no longer has the role it was admitted with.
pub async fn check_roles(state: &AppState) {
    let sockets: HashSet<_> = state.rooms.sockets().await.into_iter().collect();
    for (room_id, user, role) in sockets {
        match db::role_in(&state.pool, &room_id, user).await {
            Ok(current) if current == Some(role) => {}
            Ok(_) => {
                state
                    .rooms
                    .revoke(&room_id, user, CloseReason::AccessChanged)
                    .await
            }
            Err(error) => tracing::warn!(%error, room = %room_id, "cannot re-read a role"),
        }
    }
}

fn next_client() -> ClientId {
    NEXT_CLIENT.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_oversized_message_closes_with_1009() {
        let too_big = axum::Error::new(tungstenite::Error::Capacity(
            tungstenite::error::CapacityError::MessageTooLong {
                size: MAX_MESSAGE_BYTES + 1,
                max_size: MAX_MESSAGE_BYTES,
            },
        ));

        assert!(
            is_too_large(&too_big),
            "a capacity error is an oversized message"
        );
        assert_eq!(CloseReason::TooLarge.code(), 1009);
        assert!(!is_too_large(&axum::Error::new(
            tungstenite::Error::ConnectionClosed
        )));
    }
}
