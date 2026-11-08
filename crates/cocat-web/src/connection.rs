//! The room's websocket: the sync handshake, updates both ways, and reconnecting.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use cocat_core::SUBPROTOCOL;
use js_sys::{ArrayBuffer, Uint8Array};
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{BinaryType, MessageEvent, WebSocket};
use yrs::TransactionAcqError;

use crate::presence::Presence;
use crate::session::login_url;
use crate::state::Room;
use crate::sync::{Backoff, Status, hello, is_remote, receive, update_frame};

struct Handlers {
    open: Closure<dyn FnMut()>,
    message: Closure<dyn FnMut(MessageEvent)>,
    close: Closure<dyn FnMut()>,
}

struct Link {
    room: Room,
    socket: RefCell<Option<WebSocket>>,
    /// Whether the current socket got as far as the server's state.
    synced: Cell<bool>,
    backoff: RefCell<Backoff>,
    presence: RefCell<Presence>,
    render_queued: Rc<Cell<bool>>,
    handlers: Handlers,
}

/// Keeps the room's replica in step with the server for as long as the page lives: the
/// document's observer owns the link.
pub fn connect(room: Room) -> Result<(), TransactionAcqError> {
    let link = Rc::new_cyclic(|link: &Weak<Link>| Link {
        room: room.clone(),
        socket: RefCell::new(None),
        synced: Cell::new(false),
        backoff: RefCell::new(Backoff::default()),
        presence: RefCell::new(Presence::new(room.doc.clone())),
        render_queued: Rc::default(),
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
    link.open();
    Ok(())
}

fn handlers(link: &Weak<Link>) -> Handlers {
    let (open, message, close) = (link.clone(), link.clone(), link.clone());
    Handlers {
        open: Closure::new(move || {
            if let Some(link) = open.upgrade() {
                link.send(&hello(&link.room.doc));
                link.announce(link.room.editing.get_untracked());
            }
        }),
        message: Closure::new(move |event: MessageEvent| {
            if let Some(link) = message.upgrade() {
                link.receive(&event);
            }
        }),
        close: Closure::new(move || {
            if let Some(link) = close.upgrade() {
                link.closed();
            }
        }),
    }
}

impl Link {
    fn open(self: &Rc<Self>) {
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
        }
    }

    fn closed(self: &Rc<Self>) {
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
