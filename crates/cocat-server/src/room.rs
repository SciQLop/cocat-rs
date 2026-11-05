//! One thread per open room. It owns the `Doc` and the room file; sockets reach it
//! through channels, because `Doc` and `Update` are not `Send`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use bytes::Bytes;
use cocat_core::role::Role;
use cocat_core::store::{FileRoomStore, StoreError};
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;
use yrs::sync::{Message, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, ReadTxn, Transact, Update};

pub type ClientId = u64;

type Rooms = Arc<Mutex<HashMap<String, RoomHandle>>>;

#[derive(Debug, Error)]
pub enum RoomError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("the room registry is poisoned")]
    Registry,
    #[error("cannot spawn the room task: {0}")]
    Spawn(#[source] std::io::Error),
}

/// Opens rooms on demand and unloads them when their last client leaves.
#[derive(Debug)]
pub struct RoomManager {
    update_dir: PathBuf,
    rooms: Rooms,
}

impl RoomManager {
    pub fn new(update_dir: PathBuf) -> Self {
        Self {
            update_dir,
            rooms: Arc::default(),
        }
    }

    pub fn get_or_open(&self, room_id: &str) -> Result<RoomHandle, RoomError> {
        let mut rooms = self.rooms.lock().map_err(|_| RoomError::Registry)?;
        if let Some(handle) = rooms.get(room_id) {
            return Ok(handle.clone());
        }
        let (store, updates) = FileRoomStore::open(&self.update_dir, room_id)?;
        let (commands, receiver) = mpsc::channel();
        let registry = Arc::clone(&self.rooms);
        let task_room_id = room_id.to_owned();
        thread::Builder::new()
            .name(format!("room-{room_id}"))
            .spawn(move || room_loop(task_room_id, store, updates, receiver, registry))
            .map_err(RoomError::Spawn)?;
        let handle = RoomHandle { commands };
        rooms.insert(room_id.to_owned(), handle.clone());
        Ok(handle)
    }

    pub fn open_rooms(&self) -> usize {
        self.rooms.lock().map(|rooms| rooms.len()).unwrap_or(0)
    }
}

#[derive(Debug, Clone)]
pub struct RoomHandle {
    commands: Sender<RoomCommand>,
}

impl RoomHandle {
    pub fn attach(&self, client: ClientId, role: Role, out: UnboundedSender<Bytes>) {
        let _ = self
            .commands
            .send(RoomCommand::Attach { client, role, out });
    }

    pub fn frame(&self, client: ClientId, frame: Bytes) {
        let _ = self.commands.send(RoomCommand::Frame { client, frame });
    }

    /// simplify: waits for the room thread to process the detach, so the caller (and the
    /// tests) can rely on the last detach having flushed; phase 1 gets an async handshake.
    pub fn detach(&self, client: ClientId) {
        let (done, processed) = mpsc::channel();
        if self
            .commands
            .send(RoomCommand::Detach { client, done })
            .is_ok()
        {
            let _ = processed.recv();
        }
    }
}

#[derive(Debug)]
enum RoomCommand {
    Attach {
        client: ClientId,
        role: Role,
        out: UnboundedSender<Bytes>,
    },
    Frame {
        client: ClientId,
        frame: Bytes,
    },
    Detach {
        client: ClientId,
        done: Sender<()>,
    },
}

struct Client {
    role: Role,
    out: UnboundedSender<Bytes>,
}

fn room_loop(
    room_id: String,
    mut store: FileRoomStore,
    updates: Vec<Bytes>,
    commands: Receiver<RoomCommand>,
    registry: Rooms,
) {
    let doc = Doc::new();
    let mut clients = HashMap::new();
    for update in updates {
        apply(&doc, &update);
    }

    let mut unloaded = None;
    while let Ok(command) = commands.recv() {
        match command {
            RoomCommand::Attach { client, role, out } => {
                attach(&doc, &mut clients, client, Client { role, out });
            }
            RoomCommand::Frame { client, frame } => {
                handle_frame(&doc, &mut store, &clients, client, frame);
            }
            RoomCommand::Detach { client, done } => {
                clients.remove(&client);
                if clients.is_empty() {
                    unloaded = Some(done);
                    break;
                }
                let _ = done.send(());
            }
        }
    }

    if let Err(error) = store.flush() {
        tracing::warn!(room = %room_id, %error, "cannot flush the room on unload");
    }
    match registry.lock() {
        Ok(mut rooms) => {
            rooms.remove(&room_id);
        }
        Err(_) => tracing::error!(room = %room_id, "the room registry is poisoned"),
    }
    if let Some(done) = unloaded {
        let _ = done.send(());
    }
}

fn attach(doc: &Doc, clients: &mut HashMap<ClientId, Client>, client: ClientId, entry: Client) {
    let step = Message::Sync(SyncMessage::SyncStep1(doc.transact().state_vector())).encode_v1();
    if entry.out.send(Bytes::from(step)).is_err() {
        return;
    }
    clients.insert(client, entry);
}

fn handle_frame(
    doc: &Doc,
    store: &mut FileRoomStore,
    clients: &HashMap<ClientId, Client>,
    client: ClientId,
    frame: Bytes,
) {
    if frame.len() < 2 {
        return;
    }
    let Some(sender) = clients.get(&client) else {
        return;
    };
    let message = match Message::decode_v1(&frame) {
        Ok(message) => message,
        Err(error) => {
            tracing::warn!(%client, %error, "ignoring an undecodable frame");
            return;
        }
    };
    let Message::Sync(message) = message else {
        return;
    };
    match message {
        SyncMessage::SyncStep1(state_vector) => {
            let update = doc.transact().encode_state_as_update_v1(&state_vector);
            let reply = Message::Sync(SyncMessage::SyncStep2(update)).encode_v1();
            let _ = sender.out.send(Bytes::from(reply));
        }
        SyncMessage::SyncStep2(update) | SyncMessage::Update(update) => {
            if !sender.role.can_write() {
                return;
            }
            apply_and_broadcast(doc, store, clients, update);
        }
    }
}

fn apply_and_broadcast(
    doc: &Doc,
    store: &mut FileRoomStore,
    clients: &HashMap<ClientId, Client>,
    payload: Vec<u8>,
) {
    if !apply(doc, &payload) {
        return;
    }
    if let Err(error) = store.append(Bytes::from(payload.clone())) {
        tracing::warn!(%error, "cannot append to the room file, the update is in memory only");
    }
    let frame = Bytes::from(Message::Sync(SyncMessage::Update(payload)).encode_v1());
    for client in clients.values() {
        let _ = client.out.send(frame.clone());
    }
}

fn apply(doc: &Doc, payload: &[u8]) -> bool {
    let update = match Update::decode_v1(payload) {
        Ok(update) => update,
        Err(error) => {
            tracing::warn!(%error, "skipping a record that is not a v1 update");
            return false;
        }
    };
    match doc.transact_mut().apply_update(update) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, "skipping a record that does not apply");
            false
        }
    }
}
