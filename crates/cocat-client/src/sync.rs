//! The background loop that keeps one room in step with the server: the Yjs sync handshake,
//! updates both ways, and a reconnect with a backoff after anything but a refusal.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use cocat_core::role::Role;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::time::{Instant, MissedTickBehavior};
use tokio_tungstenite::tungstenite::Error as SocketError;
use tokio_tungstenite::tungstenite::Message as Frame;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Response;
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode, header};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async_with_config};
use url::Url;
use yrs::sync::{Message, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{ReadTxn, Transact, Update};

use crate::login::Credentials;
use crate::room::{Inner, Outgoing, REMOTE_ORIGIN, RoomError, Status};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The first wait after a dropped session, doubling up to [`MAX_BACKOFF`].
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// How often a session pings the server, and how long a synced session waits for any frame
/// (a Pong at least) before it counts the connection as dead and reconnects. Nothing else
/// notices a half-open connection, as after a laptop slept and woke up on another network:
/// the socket just stops delivering, and only a write fails, after the kernel's minutes of
/// retransmits.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Heartbeat {
    pub every: Duration,
    pub silence: Duration,
}

pub(crate) const HEARTBEAT: Heartbeat = Heartbeat {
    every: Duration::from_secs(20),
    silence: Duration::from_secs(45),
};

/// The silence allowed until the room's state has arrived. The server sends that state as
/// one message, and nothing reaches the client before all of it has: a Pong waits behind it
/// on the same stream. A big project (100 MB) on a slow link takes minutes.
const FIRST_STATE_SILENCE: Duration = Duration::from_secs(300);

/// The largest message the client reads. The server sends a room's whole state in one
/// message, and a relayed catalogue (LASCO's CMEs) is ~20 MiB, past tungstenite's 16 MiB.
const MAX_MESSAGE_BYTES: usize = 256 * 1024 * 1024;

/// Keeps one room connected until the server refuses the session or the project.
///
/// A refusal with credentials logs in once and tries again; a refusal of the cookie that
/// login just gave, or a failed login, is final. A changed access reconnects at once, to
/// learn the new role, and a project the account left is final.
pub(crate) async fn run(
    inner: Arc<Inner>,
    url: Url,
    mut cookie: String,
    credentials: Option<Credentials>,
) {
    let mut backoff = INITIAL_BACKOFF;
    let mut fresh_login = false;
    loop {
        inner.status.set(Status::Connecting);
        let ending = session(&inner, &url, &cookie).await;
        // No socket, so nothing can be sent: a local edit belongs to the handshake diff.
        inner.online.store(false, Ordering::Relaxed);
        match ending {
            Ending::Refused => {
                let Some(credentials) = &credentials else {
                    inner.status.set(Status::Unauthorized);
                    return;
                };
                if fresh_login {
                    inner.status.set(Status::Unauthorized);
                    return;
                }
                match log_in_again(&url, credentials).await {
                    Some(renewed) => {
                        cookie = renewed;
                        fresh_login = true;
                        backoff = INITIAL_BACKOFF;
                        continue;
                    }
                    None => {
                        inner.status.set(Status::Unauthorized);
                        return;
                    }
                }
            }
            Ending::NoAccess => {
                inner.status.set(Status::NoAccess);
                return;
            }
            // Only after a synced session: one closed before it synced could be a server
            // that keeps closing, and that is what the backoff is for.
            Ending::AccessChanged { synced: true } => {
                backoff = INITIAL_BACKOFF;
                fresh_login = false;
                continue;
            }
            Ending::AccessChanged { synced } | Ending::Lost { synced } => {
                if synced {
                    backoff = INITIAL_BACKOFF;
                    // The cookie opened a session, so a refusal from now on is news.
                    fresh_login = false;
                }
            }
        }
        // The status says "connecting" through the wait: a caller waiting for a reconnect
        // should not see the room look offline while the backoff runs.
        inner.status.set(Status::Connecting);
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// Logs in with the credentials the caller gave, without ever putting them in a log line.
async fn log_in_again(url: &Url, credentials: &Credentials) -> Option<String> {
    let Some(base) = http_base(url) else {
        tracing::warn!("the room url has no http form to log in on");
        return None;
    };
    match crate::login::login(base.as_str(), &credentials.username, &credentials.password).await {
        Ok(cookie) => Some(cookie),
        Err(error) => {
            tracing::warn!(%error, "the session was refused and logging in failed");
            None
        }
    }
}

/// The server's address as a directory: the websocket url without the room it names.
fn http_base(url: &Url) -> Option<Url> {
    let mut base = url.clone();
    let scheme = match base.scheme() {
        "ws" => "http",
        "wss" => "https",
        _ => return None,
    };
    base.set_scheme(scheme).ok()?;
    {
        let mut segments = base.path_segments_mut().ok()?;
        segments.pop();
        segments.pop();
    }
    Some(base)
}

/// How one connection ended.
enum Ending {
    /// The server will not accept this cookie; retrying would only hammer it.
    Refused,
    /// The session is good but the account is not in this project.
    NoAccess,
    /// The server closed the socket because the account's role or membership changed.
    AccessChanged { synced: bool },
    /// The connection closed or broke; `synced` says whether it had got as far as exchanging
    /// state, which decides whether the backoff starts over.
    Lost { synced: bool },
}

async fn session(inner: &Arc<Inner>, url: &Url, cookie: &str) -> Ending {
    let Some(request) = request(url, cookie) else {
        return Ending::Lost { synced: false };
    };
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES));
    let (mut socket, role) = match connect_async_with_config(request, Some(config), false).await {
        Ok((socket, response)) => (socket, announced_role(&response)),
        Err(SocketError::Http(response)) => {
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED {
                return Ending::Refused;
            }
            if status == StatusCode::FORBIDDEN {
                return Ending::NoAccess;
            }
            tracing::warn!(%status, "the server refused the websocket handshake");
            return Ending::Lost { synced: false };
        }
        Err(error) => {
            tracing::warn!(%error, "cannot reach the room");
            return Ending::Lost { synced: false };
        }
    };

    // A server that names a role sends its state vector first; a read-only room reads it
    // before saying hello, so the hello already describes the document it keeps.
    let mut first = None;
    if let Some(role) = role {
        inner.access.learn(role);
        if !role.can_write() {
            let Ok(frame) = tokio::time::timeout(FIRST_STATE_SILENCE, socket.next()).await else {
                tracing::warn!("the server sent nothing after the handshake");
                return Ending::Lost { synced: false };
            };
            if let Some(Ok(Frame::Binary(bytes))) = &frame
                && let Err(error) = drop_unshared_changes(inner, bytes)
            {
                tracing::error!(%error, "the read-only room could not start over");
                return Ending::Lost { synced: false };
            }
            first = Some(frame);
        }
    }

    let hello = state_vector(inner);
    if let Err(error) = socket.send(Frame::Binary(hello.into())).await {
        tracing::warn!(%error, "the room could not say hello");
        return Ending::Lost { synced: false };
    }
    inner.online.store(true, Ordering::Relaxed);

    let heartbeat = *crate::room::lock(&inner.heartbeat);
    let mut pings = tokio::time::interval_at(Instant::now() + heartbeat.every, heartbeat.every);
    pings.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut heard = Instant::now();
    let mut synced = false;
    let mut awaiting_ack = false;
    loop {
        tokio::select! {
            frame = next_frame(&mut first, &mut socket) => {
                heard = Instant::now();
                let Some(frame) = frame else {
                    return Ending::Lost { synced };
                };
                match frame {
                    Ok(Frame::Binary(bytes)) => {
                        for reply in receive(inner, &bytes, &mut synced, &mut awaiting_ack) {
                            if let Err(error) = socket.send(Frame::Binary(reply.into())).await {
                                tracing::warn!(%error, "the room could not answer the server");
                                return Ending::Lost { synced };
                            }
                        }
                    }
                    Ok(Frame::Close(close)) => {
                        return closed(close.map(|frame| frame.code), synced);
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(%error, "the room lost its connection");
                        return Ending::Lost { synced };
                    }
                }
            }
            () = inner.outbox.ready() => {
                for outgoing in inner.outbox.drain() {
                    let frame = match outgoing {
                        Outgoing::Update(update) => {
                            Message::Sync(SyncMessage::Update(update)).encode_v1()
                        }
                        Outgoing::Ack => {
                            awaiting_ack = true;
                            state_vector(inner)
                        }
                    };
                    if let Err(error) = socket.send(Frame::Binary(frame.into())).await {
                        tracing::warn!(%error, "the room could not send a local change");
                        return Ending::Lost { synced };
                    }
                }
            }
            _ = pings.tick() => {
                let silence = if synced { heartbeat.silence } else { FIRST_STATE_SILENCE };
                if heard.elapsed() > silence {
                    tracing::warn!(room = %inner.room_id, "the server went silent; reconnecting");
                    return Ending::Lost { synced };
                }
                // A connection whose peer is gone stops taking bytes once the buffers fill:
                // a send that hangs is a dead connection too.
                let ping = socket.send(Frame::Ping(Default::default()));
                if !matches!(tokio::time::timeout(heartbeat.silence, ping).await, Ok(Ok(()))) {
                    tracing::warn!(room = %inner.room_id, "the server took no ping; reconnecting");
                    return Ending::Lost { synced };
                }
            }
        }
    }
}

/// How a close frame ends the session: `1008` is a session the server ended, the access
/// code a role or membership that changed, anything else a connection to retry.
fn closed(code: Option<CloseCode>, synced: bool) -> Ending {
    match code.map(u16::from) {
        Some(code) if code == u16::from(CloseCode::Policy) => Ending::Refused,
        Some(cocat_core::ACCESS_CHANGED_CLOSE) => Ending::AccessChanged { synced },
        _ => Ending::Lost { synced },
    }
}

/// The role the upgrade answer names; `None` from a server that does not say.
fn announced_role(response: &Response) -> Option<Role> {
    response
        .headers()
        .get(cocat_core::ROLE_HEADER)?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

/// The frame read ahead of the hello, if any, before the socket's next one.
async fn next_frame(
    first: &mut Option<Option<Result<Frame, SocketError>>>,
    socket: &mut Socket,
) -> Option<Result<Frame, SocketError>> {
    match first.take() {
        Some(frame) => frame,
        None => socket.next().await,
    }
}

/// A read-only room whose document holds changes the server lacks (written before the role
/// was known, or by an older client) starts over from the server's copy: the server drops
/// those changes, so they would otherwise stay in the cache forever.
// simplify: only insertions show in a state vector; a viewer's pure deletion made before the
// role was known stays local. Comparing delete sets needs the server's full state; fetch it
// over the history API's state route if that case matters.
fn drop_unshared_changes(inner: &Arc<Inner>, server_hello: &[u8]) -> Result<(), RoomError> {
    let Ok(Message::Sync(SyncMessage::SyncStep1(server))) = Message::decode_v1(server_hello) else {
        return Ok(());
    };
    let local = inner.doc().transact().state_vector();
    let unshared = local
        .iter()
        .any(|(client, clock)| *clock > server.get(client));
    if !unshared {
        return Ok(());
    }
    tracing::warn!(
        room = %inner.room_id,
        "this account may only read the project; dropping the local changes the server never took"
    );
    inner.start_over()
}

/// The handshake request: the websocket address, the v2 subprotocol and the session, as a
/// cookie and a bearer token (see `login::authorized`).
fn request(
    url: &Url,
    cookie: &str,
) -> Option<tokio_tungstenite::tungstenite::handshake::client::Request> {
    let mut request = url.as_str().into_client_request().ok()?;
    let headers = request.headers_mut();
    headers.insert(
        header::SEC_WEBSOCKET_PROTOCOL,
        HeaderValue::from_static(cocat_core::SUBPROTOCOL),
    );
    let as_cookie = HeaderValue::from_str(&crate::login::cookie_header(cookie));
    let as_bearer = HeaderValue::from_str(&format!("Bearer {cookie}"));
    match (as_cookie, as_bearer) {
        (Ok(cookie), Ok(bearer)) => {
            headers.insert(header::COOKIE, cookie);
            headers.insert(header::AUTHORIZATION, bearer);
            Some(request)
        }
        (Err(error), _) | (_, Err(error)) => {
            tracing::warn!(%error, "the session cannot be sent in a header");
            None
        }
    }
}

/// Handles one frame from the server, returning the frames the protocol asks for.
fn receive(
    inner: &Inner,
    bytes: &[u8],
    synced: &mut bool,
    awaiting_ack: &mut bool,
) -> Vec<Vec<u8>> {
    let Ok(Message::Sync(message)) = Message::decode_v1(bytes) else {
        // Awareness and custom frames are not part of the room protocol.
        return Vec::new();
    };
    match message {
        SyncMessage::SyncStep1(wanted) => {
            // Whatever is queued is about to be inside the diff below, which is computed after
            // this point: dropping it here sends it once, not twice. Anything pushed later is
            // still sent as its own update.
            let queued = inner.outbox.drain();
            let reply = inner.doc().transact().encode_state_as_update_v1(&wanted);
            let mut frames = vec![Message::Sync(SyncMessage::SyncStep2(reply)).encode_v1()];
            // An acknowledgement asked for before the server spoke still has to be asked for,
            // and only after the diff above: the answer proves that diff was applied too.
            if queued.iter().any(|item| matches!(item, Outgoing::Ack)) {
                *awaiting_ack = true;
                frames.push(state_vector(inner));
            }
            frames
        }
        SyncMessage::SyncStep2(update) => {
            apply(inner, &update);
            if *awaiting_ack {
                *awaiting_ack = false;
                inner.ack.acknowledge();
            }
            if !*synced {
                *synced = true;
                inner.status.set(Status::Synced);
            }
            Vec::new()
        }
        SyncMessage::Update(update) => {
            apply(inner, &update);
            Vec::new()
        }
    }
}

/// The frame that asks the server what it still needs, and whose answer proves our updates
/// before it have been processed.
fn state_vector(inner: &Inner) -> Vec<u8> {
    let state = inner.doc().transact().state_vector();
    Message::Sync(SyncMessage::SyncStep1(state)).encode_v1()
}

/// Applies what the server sent as one remote transaction, so its changes are one batch.
fn apply(inner: &Inner, bytes: &[u8]) {
    let update = match Update::decode_v1(bytes) {
        Ok(update) => update,
        Err(error) => {
            tracing::warn!(%error, "the server sent an update the codec refused");
            return;
        }
    };
    if let Err(error) = inner
        .doc()
        .transact_mut_with(REMOTE_ORIGIN)
        .apply_update(update)
    {
        tracing::warn!(%error, "the document refused the server's update");
    }
}
