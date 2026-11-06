//! The background loop that keeps one room in step with the server: the Yjs sync handshake,
//! updates both ways, and a reconnect with a backoff after anything but a refusal.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Error as SocketError;
use tokio_tungstenite::tungstenite::Message as Frame;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode, header};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use url::Url;
use yrs::sync::{Message, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{ReadTxn, Transact, Update};

use crate::room::{Inner, REMOTE_ORIGIN, Status};

/// The first wait after a dropped session, doubling up to [`MAX_BACKOFF`].
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Keeps one room connected until the server refuses the session.
pub(crate) async fn run(inner: Arc<Inner>, url: Url, cookie: String) {
    let mut backoff = INITIAL_BACKOFF;
    loop {
        inner.status.set(Status::Connecting);
        match session(&inner, &url, &cookie).await {
            Ending::Refused => {
                inner.status.set(Status::Unauthorized);
                return;
            }
            Ending::Lost { synced } => {
                if synced {
                    backoff = INITIAL_BACKOFF;
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

/// How one connection ended.
enum Ending {
    /// The server will not accept this cookie; retrying would only hammer it.
    Refused,
    /// The connection closed or broke; `synced` says whether it had got as far as exchanging
    /// state, which decides whether the backoff starts over.
    Lost { synced: bool },
}

async fn session(inner: &Arc<Inner>, url: &Url, cookie: &str) -> Ending {
    let Some(request) = request(url, cookie) else {
        return Ending::Lost { synced: false };
    };
    let mut socket = match connect_async(request).await {
        Ok((socket, _)) => socket,
        Err(SocketError::Http(response)) => {
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                return Ending::Refused;
            }
            tracing::warn!(%status, "the server refused the websocket handshake");
            return Ending::Lost { synced: false };
        }
        Err(error) => {
            tracing::warn!(%error, "cannot reach the room");
            return Ending::Lost { synced: false };
        }
    };

    let hello = {
        let txn = inner.doc.transact();
        Message::Sync(SyncMessage::SyncStep1(txn.state_vector())).encode_v1()
    };
    if let Err(error) = socket.send(Frame::Binary(hello.into())).await {
        tracing::warn!(%error, "the room could not say hello");
        return Ending::Lost { synced: false };
    }

    let mut synced = false;
    loop {
        tokio::select! {
            frame = socket.next() => {
                let Some(frame) = frame else {
                    return Ending::Lost { synced };
                };
                match frame {
                    Ok(Frame::Binary(bytes)) => {
                        if let Some(reply) = receive(inner, &bytes, &mut synced)
                            && let Err(error) = socket.send(Frame::Binary(reply.into())).await
                        {
                            tracing::warn!(%error, "the room could not answer the server");
                            return Ending::Lost { synced };
                        }
                    }
                    Ok(Frame::Close(close)) => {
                        return if close.as_ref().map(|frame| frame.code) == Some(CloseCode::Policy)
                        {
                            Ending::Refused
                        } else {
                            Ending::Lost { synced }
                        };
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(%error, "the room lost its connection");
                        return Ending::Lost { synced };
                    }
                }
            }
            () = inner.outbox.ready() => {
                for update in inner.outbox.drain() {
                    let frame = Message::Sync(SyncMessage::Update(update)).encode_v1();
                    if let Err(error) = socket.send(Frame::Binary(frame.into())).await {
                        tracing::warn!(%error, "the room could not send a local change");
                        return Ending::Lost { synced };
                    }
                }
            }
        }
    }
}

/// The handshake request: the websocket address, and the session cookie the server reads.
fn request(
    url: &Url,
    cookie: &str,
) -> Option<tokio_tungstenite::tungstenite::handshake::client::Request> {
    let mut request = url.as_str().into_client_request().ok()?;
    let value = crate::login::cookie_header(cookie);
    match HeaderValue::from_str(&value) {
        Ok(value) => {
            request.headers_mut().insert(header::COOKIE, value);
            Some(request)
        }
        Err(error) => {
            tracing::warn!(%error, "the cookie cannot be sent in a header");
            None
        }
    }
}

/// Handles one frame from the server, answering with the frame the protocol asks for.
fn receive(inner: &Inner, bytes: &[u8], synced: &mut bool) -> Option<Vec<u8>> {
    let Ok(Message::Sync(message)) = Message::decode_v1(bytes) else {
        // Awareness and custom frames are not part of the room protocol.
        return None;
    };
    match message {
        SyncMessage::SyncStep1(wanted) => {
            let txn = inner.doc.transact();
            let reply = txn.encode_state_as_update_v1(&wanted);
            Some(Message::Sync(SyncMessage::SyncStep2(reply)).encode_v1())
        }
        SyncMessage::SyncStep2(update) => {
            apply(inner, &update);
            if !*synced {
                *synced = true;
                inner.status.set(Status::Synced);
            }
            None
        }
        SyncMessage::Update(update) => {
            apply(inner, &update);
            None
        }
    }
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
        .doc
        .transact_mut_with(REMOTE_ORIGIN)
        .apply_update(update)
    {
        tracing::warn!(%error, "the document refused the server's update");
    }
}
