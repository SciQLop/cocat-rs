//! The room's websocket: the sync handshake, updates both ways, and reconnecting.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use cocat_core::{STATE_BYTES_HEADER, SUBPROTOCOL};
use js_sys::{ArrayBuffer, Promise, Reflect, Uint8Array};
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen_futures::JsFuture;
use web_sys::{BinaryType, CloseEvent, MessageEvent, ReadableStreamDefaultReader, WebSocket};
use yrs::TransactionAcqError;

use crate::presence::Presence;
use crate::projects::api_url;
use crate::session::login_url;
use crate::state::Room;
use crate::sync::{
    AfterClose, Backoff, Loading, Status, after_close, apply_state, hello, is_remote, receive,
    update_frame,
};

struct Handlers {
    open: Closure<dyn FnMut()>,
    message: Closure<dyn FnMut(MessageEvent)>,
    close: Closure<dyn FnMut(CloseEvent)>,
}

struct Link {
    room: Room,
    socket: RefCell<Option<WebSocket>>,
    /// Whether the current socket got as far as the server's state.
    synced: Cell<bool>,
    /// While the page downloads the project's state, the socket does not say hello yet:
    /// an empty replica's hello would ask for the whole state again.
    handshake_held: Cell<bool>,
    backoff: RefCell<Backoff>,
    presence: RefCell<Presence>,
    render_queued: Rc<Cell<bool>>,
    /// Set once the page moved to another project: no more sockets.
    stopped: Cell<bool>,
    /// How many times the socket closed, so a caller can tell a refusal happened.
    drops: Cell<u32>,
    handlers: Handlers,
}

/// The room's link to the server, until [`Connection::close`].
pub struct Connection(Rc<Link>);

impl Connection {
    /// Ends the link for good: the socket closes, no reconnect follows, and the document's
    /// observer lets go of the link.
    pub fn close(&self) {
        self.0.stop();
    }

    /// Asks the server for its state again: it answers after every frame sent before, so
    /// the room goes Live again once the server has applied them.
    pub fn resync(&self) {
        self.0.room.status.set(Status::Connecting);
        self.0.send(&hello(&self.0.room.doc));
    }

    /// How many times the socket closed so far; the server closes it on a refused update.
    pub fn drops(&self) -> u32 {
        self.0.drops.get()
    }
}

/// Keeps the room's replica in step with the server until the connection closes: the
/// document's observer owns the link.
pub fn connect(room: Room) -> Result<Connection, TransactionAcqError> {
    let link = linked(room)?;
    link.open();
    Ok(Connection(link))
}

/// Like [`connect`], for a project's catalogues: the page downloads the project's state over
/// HTTP, compressed and with its progress shown, while the socket opens; the socket's
/// handshake waits for the download, so it brings only what changed since. A failed
/// download leaves it all to the socket.
pub fn connect_project(room: Room) -> Result<Connection, TransactionAcqError> {
    let link = linked(room)?;
    link.handshake_held.set(true);
    link.open();
    link.room.loading.set(Some(Loading::Downloading {
        received: 0,
        total: None,
    }));
    let loader = link.clone();
    leptos::task::spawn_local(async move {
        if let Some(state) = download(&loader.room).await {
            loader.room.loading.set(Some(Loading::Opening));
            // The note says "Opening…" before reading the state holds the page.
            next_frame().await;
            next_frame().await;
            if apply_state(&loader.room.doc, &state) {
                loader.room.loading.set(None);
            }
        }
        loader.release_handshake();
    });
    Ok(Connection(link))
}

/// The project's state, or `None` when the server cannot give it (an older server, a
/// proxy's error): the socket's handshake brings it then.
async fn download(room: &Room) -> Option<Vec<u8>> {
    let url = api_url(
        &room.config.api_url,
        &["rooms", &room.config.room_id, "state"],
    )?;
    let response = gloo_net::http::Request::get(&url).send().await.ok()?;
    if !response.ok() {
        return None;
    }
    let total = response
        .headers()
        .get(STATE_BYTES_HEADER)
        .and_then(|size| size.parse::<u64>().ok());
    let reader: ReadableStreamDefaultReader = response.body()?.get_reader().unchecked_into();
    let mut state = Vec::with_capacity(total.unwrap_or(0).try_into().ok()?);
    loop {
        let chunk = JsFuture::from(reader.read()).await.ok()?;
        if Reflect::get(&chunk, &"done".into()).ok()?.is_truthy() {
            return Some(state);
        }
        let bytes: Uint8Array = Reflect::get(&chunk, &"value".into()).ok()?.unchecked_into();
        let start = state.len();
        state.resize(start + bytes.length() as usize, 0);
        bytes.copy_to(&mut state[start..]);
        room.loading.set(Some(Loading::Downloading {
            received: state.len() as u64,
            total,
        }));
    }
}

async fn next_frame() {
    let frame = Promise::new(&mut |resolve, _| {
        let _ = window().request_animation_frame(&resolve);
    });
    let _ = JsFuture::from(frame).await;
}

fn linked(room: Room) -> Result<Rc<Link>, TransactionAcqError> {
    let link = Rc::new_cyclic(|link: &Weak<Link>| Link {
        room: room.clone(),
        socket: RefCell::new(None),
        synced: Cell::new(false),
        handshake_held: Cell::new(false),
        backoff: RefCell::new(Backoff::default()),
        presence: RefCell::new(Presence::for_room(room.doc.clone(), &room.config.room_id)),
        render_queued: Rc::default(),
        stopped: Cell::new(false),
        drops: Cell::new(0),
        handlers: handlers(link),
    });
    let observer = link.clone();
    room.doc.observe_update_v1("browser", move |txn, event| {
        if !is_remote(txn) {
            observer.send(&update_frame(&event.update));
        }
        observer.queue_render();
    })?;
    let announcer = link.clone();
    Effect::new(move |_| announcer.announce(room.editing.get()));
    Ok(link)
}

fn handlers(link: &Weak<Link>) -> Handlers {
    let (open, message, close) = (link.clone(), link.clone(), link.clone());
    Handlers {
        open: Closure::new(move || {
            if let Some(link) = open.upgrade() {
                link.say_hello();
                link.announce(link.room.editing.get_untracked());
            }
        }),
        message: Closure::new(move |event: MessageEvent| {
            if let Some(link) = message.upgrade() {
                link.receive(&event);
            }
        }),
        close: Closure::new(move |event: CloseEvent| {
            let Some(link) = close.upgrade() else {
                return;
            };
            match after_close(event.code()) {
                AfterClose::Reload => {
                    let _ = window().location().reload();
                }
                AfterClose::Reconnect => link.closed(),
            }
        }),
    }
}

impl Link {
    fn open(self: &Rc<Self>) {
        if self.stopped.get() {
            return;
        }
        let Ok(socket) = WebSocket::new_with_str(&self.room.config.ws_url, SUBPROTOCOL) else {
            self.closed();
            return;
        };
        socket.set_binary_type(BinaryType::Arraybuffer);
        socket.set_onopen(Some(self.handlers.open.as_ref().unchecked_ref()));
        socket.set_onmessage(Some(self.handlers.message.as_ref().unchecked_ref()));
        socket.set_onclose(Some(self.handlers.close.as_ref().unchecked_ref()));
        self.socket.replace(Some(socket));
    }

    fn say_hello(&self) {
        if !self.handshake_held.get() {
            self.send(&hello(&self.room.doc));
        }
    }

    /// The downloaded state is in (or the download failed): the handshake can ask for
    /// the rest.
    fn release_handshake(&self) {
        self.handshake_held.set(false);
        self.say_hello();
    }

    /// Sends a frame when the socket is open. Otherwise the next handshake carries the
    /// change: the server's state vector asks for everything it lacks.
    fn send(&self, frame: &[u8]) {
        if let Some(socket) = self.socket.borrow().as_ref()
            && socket.ready_state() == WebSocket::OPEN
        {
            let _ = socket.send_with_u8_array(frame);
        }
    }

    /// Tells the room what this page is editing; the next handshake repeats it.
    fn announce(&self, editing: Option<String>) {
        if let Some(frame) = self.presence.borrow_mut().announce(editing) {
            self.send(&frame);
        }
    }

    fn receive(&self, event: &MessageEvent) {
        let Ok(buffer) = event.data().dyn_into::<ArrayBuffer>() else {
            return;
        };
        let bytes = Uint8Array::new(&buffer).to_vec();
        if self.presence.borrow_mut().receive(&bytes) {
            self.room.peers.set(self.presence.borrow().peers());
            return;
        }
        let reception = receive(&self.room.doc, &bytes);
        for reply in &reception.replies {
            self.send(reply);
        }
        if reception.synced {
            // The first announce can reach the server while it still holds this page's old
            // socket, and its snapshot can carry this page's stale state: saying it again
            // once the room answered settles both.
            if !self.synced.replace(true) {
                self.announce(self.room.editing.get_untracked());
            }
            self.backoff.borrow_mut().reset();
            self.room.status.set(Status::Live);
            self.room.loading.set(None);
        }
    }

    fn closed(self: &Rc<Self>) {
        self.drops.set(self.drops.get() + 1);
        self.socket.replace(None);
        self.room.status.set(Status::Reconnecting);
        self.presence.borrow_mut().disconnected();
        self.room.peers.set(Vec::new());
        if !self.synced.replace(false) {
            check_session(&self.room);
        }
        let delay = self.backoff.borrow_mut().next_delay();
        let link = self.clone();
        set_timeout(move || link.open(), delay);
    }

    fn stop(&self) {
        self.stopped.set(true);
        let _ = self.room.doc.unobserve_update_v1("browser");
        if let Some(socket) = self.socket.take() {
            socket.set_onclose(None);
            socket.set_onmessage(None);
            close_when_open(socket);
        }
    }

    /// Updates come in bursts; the views read the replica once per frame at most.
    fn queue_render(&self) {
        if self.render_queued.replace(true) {
            return;
        }
        let (room, queued) = (self.room.clone(), self.render_queued.clone());
        request_animation_frame(move || {
            queued.set(false);
            room.changed();
        });
    }
}

/// A socket closed while it is still connecting makes the browser log an error, and the
/// page leaves that early now that the state comes over HTTP: such a socket closes once open.
fn close_when_open(socket: WebSocket) {
    if socket.ready_state() != WebSocket::CONNECTING {
        socket.set_onopen(None);
        let _ = socket.close();
        return;
    }
    let opened = socket.clone();
    let close = Closure::once_into_js(move || {
        let _ = opened.close();
    });
    socket.set_onopen(Some(close.unchecked_ref()));
}

/// A browser hides why a websocket handshake failed. When the session has expired, the
/// server's API says so, and the visitor signs in again and comes back here.
fn check_session(room: &Room) {
    let me = format!("{}/me", room.config.api_url);
    leptos::task::spawn_local(async move {
        let expired = gloo_net::http::Request::get(&me)
            .send()
            .await
            .is_ok_and(|response| response.status() == 401);
        if expired {
            sign_in_again();
        }
    });
}

fn sign_in_again() {
    let location = window().location();
    let here = format!(
        "{}{}",
        location.pathname().unwrap_or_default(),
        location.hash().unwrap_or_default()
    );
    let base = document().base_uri().ok().flatten().unwrap_or_default();
    if let Ok(login) = login_url(&base, &here) {
        let _ = location.set_href(&login);
    }
}
