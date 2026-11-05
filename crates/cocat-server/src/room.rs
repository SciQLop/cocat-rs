//! One thread per open room. It owns the `Doc` and the room file; sockets reach it
//! through channels, because `Doc` and `Update` are not `Send`.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use bytes::Bytes;
use cocat_core::role::Role;
use cocat_core::store::{FileRoomStore, StoreError};
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use yrs::sync::{Message, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, ReadTxn, Transact, Update};

pub type ClientId = u64;

type Rooms = Arc<Mutex<HashMap<String, RoomHandle>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("the room unloaded before the client attached")]
pub struct RoomGone;

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
#[derive(Debug, Clone)]
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

    /// Flushes every open room, waiting for each file write to reach the disk.
    pub async fn flush_all(&self) {
        let handles: Vec<RoomHandle> = match self.rooms.lock() {
            Ok(rooms) => rooms.values().cloned().collect(),
            Err(_) => {
                tracing::error!("the room registry is poisoned");
                return;
            }
        };
        for handle in handles {
            handle.flush().await;
        }
    }
}

#[derive(Debug, Clone)]
pub struct RoomHandle {
    commands: Sender<RoomCommand>,
}

impl RoomHandle {
    pub fn attach(
        &self,
        client: ClientId,
        role: Role,
        out: UnboundedSender<Bytes>,
    ) -> Result<(), RoomGone> {
        self.commands
            .send(RoomCommand::Attach { client, role, out })
            .map_err(|_| RoomGone)
    }

    pub fn frame(&self, client: ClientId, frame: Bytes) {
        let _ = self.commands.send(RoomCommand::Frame { client, frame });
    }

    /// Waits for the room thread to process the detach, so the last detach has flushed
    /// and left the registry by the time this returns.
    pub async fn detach(&self, client: ClientId) {
        let (done, processed) = oneshot::channel();
        if self
            .commands
            .send(RoomCommand::Detach { client, done })
            .is_ok()
        {
            let _ = processed.await;
        }
    }

    /// Waits for the room thread to flush its pending records to the disk.
    pub async fn flush(&self) {
        let (done, flushed) = oneshot::channel();
        if self.commands.send(RoomCommand::Flush { done }).is_ok() {
            let _ = flushed.await;
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
        done: oneshot::Sender<()>,
    },
    Flush {
        done: oneshot::Sender<()>,
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
    let mut backlog = VecDeque::new();
    let mut commands = Some(commands);
    for update in updates {
        apply(&doc, &update);
    }

    'serving: loop {
        let unloaded = loop {
            let Some(command) = next_command(&commands, &mut backlog) else {
                break None;
            };
            let mut broadcast = None;
            match command {
                RoomCommand::Attach { client, role, out } => {
                    attach(&doc, &mut clients, client, Client { role, out });
                }
                RoomCommand::Frame { client, frame } => {
                    broadcast = handle_frame(&doc, &mut store, &clients, client, frame);
                }
                RoomCommand::Detach { client, done } => {
                    clients.remove(&client);
                    if clients.is_empty() {
                        break Some(done);
                    }
                    let _ = done.send(());
                }
                RoomCommand::Flush { done } => {
                    flush(&mut store, &room_id);
                    let _ = done.send(());
                }
            }
            drain(&commands, &mut backlog);
            if backlog.is_empty() {
                flush(&mut store, &room_id);
            }
            if let Some(frame) = broadcast {
                broadcast_to(&clients, frame);
            }
        };

        flush(&mut store, &room_id);
        let keep_serving = close_room(&room_id, &registry, &mut commands, &mut backlog);
        if let Some(done) = unloaded {
            let _ = done.send(());
        }
        if !keep_serving {
            break 'serving;
        }
    }
}

fn next_command(
    commands: &Option<Receiver<RoomCommand>>,
    backlog: &mut VecDeque<RoomCommand>,
) -> Option<RoomCommand> {
    if let Some(command) = backlog.pop_front() {
        return Some(command);
    }
    commands.as_ref()?.recv().ok()
}

fn drain(commands: &Option<Receiver<RoomCommand>>, backlog: &mut VecDeque<RoomCommand>) {
    if let Some(receiver) = commands {
        while let Ok(command) = receiver.try_recv() {
            backlog.push_back(command);
        }
    }
}

/// Returns true when a command arrived while the registry was locked, so the room must
/// keep serving; false when it removed itself and dropped its command receiver.
fn close_room(
    room_id: &str,
    registry: &Rooms,
    commands: &mut Option<Receiver<RoomCommand>>,
    backlog: &mut VecDeque<RoomCommand>,
) -> bool {
    match registry.lock() {
        Ok(mut rooms) => {
            drain(commands, backlog);
            if !backlog.is_empty() {
                return true;
            }
            rooms.remove(room_id);
            // Dropping the receiver under the registry lock turns every later attach into
            // `RoomGone`, so a handle from `get_or_open` cannot target a dying room.
            commands.take();
            false
        }
        Err(_) => {
            tracing::error!(room = %room_id, "the room registry is poisoned");
            false
        }
    }
}

fn flush(store: &mut FileRoomStore, room_id: &str) {
    if let Err(error) = store.flush() {
        tracing::warn!(room = %room_id, %error, "cannot flush the room");
    }
}

fn broadcast_to(clients: &HashMap<ClientId, Client>, frame: Bytes) {
    for client in clients.values() {
        let _ = client.out.send(frame.clone());
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
) -> Option<Bytes> {
    if frame.len() < 2 {
        return None;
    }
    let sender = clients.get(&client)?;
    let message = match Message::decode_v1(&frame) {
        Ok(message) => message,
        Err(error) => {
            tracing::warn!(%client, %error, "ignoring an undecodable frame");
            return None;
        }
    };
    let Message::Sync(message) = message else {
        return None;
    };
    match message {
        SyncMessage::SyncStep1(state_vector) => {
            let update = doc.transact().encode_state_as_update_v1(&state_vector);
            let reply = Message::Sync(SyncMessage::SyncStep2(update)).encode_v1();
            let _ = sender.out.send(Bytes::from(reply));
            None
        }
        SyncMessage::SyncStep2(update) | SyncMessage::Update(update) => {
            if !sender.role.can_write() {
                return None;
            }
            handle_update(doc, store, update)
        }
    }
}

fn handle_update(doc: &Doc, store: &mut FileRoomStore, payload: Vec<u8>) -> Option<Bytes> {
    // Python's wire_file drops this payload: an empty v1 update is two zero bytes, and
    // clients send one as their SyncStep2 on every connect.
    if payload == [0, 0] {
        return None;
    }
    if !apply(doc, &payload) {
        return None;
    }
    if let Err(error) = store.append(Bytes::from(payload.clone())) {
        tracing::warn!(%error, "cannot append to the room file, the update is in memory only");
    }
    Some(Bytes::from(
        Message::Sync(SyncMessage::Update(payload)).encode_v1(),
    ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    struct DyingRoom {
        handle: RoomHandle,
        registry: Arc<Mutex<HashMap<String, RoomHandle>>>,
        commands: Option<Receiver<RoomCommand>>,
        backlog: VecDeque<RoomCommand>,
    }

    fn dying_room() -> DyingRoom {
        let (commands, receiver) = mpsc::channel();
        let handle = RoomHandle { commands };
        let mut rooms = HashMap::new();
        rooms.insert("room".to_owned(), handle.clone());
        DyingRoom {
            handle,
            registry: Arc::new(Mutex::new(rooms)),
            commands: Some(receiver),
            backlog: VecDeque::new(),
        }
    }

    #[test]
    fn a_queued_attach_keeps_a_dying_room_alive() {
        let mut room = dying_room();
        let (out, _frames) = unbounded_channel();
        room.handle.attach(1, Role::Editor, out).unwrap();

        assert!(close_room(
            "room",
            &room.registry,
            &mut room.commands,
            &mut room.backlog
        ));
        assert_eq!(room.backlog.len(), 1);
        assert_eq!(room.registry.lock().unwrap().len(), 1);
    }

    #[test]
    fn an_idle_room_is_removed_and_later_attaches_are_refused() {
        let mut room = dying_room();

        assert!(!close_room(
            "room",
            &room.registry,
            &mut room.commands,
            &mut room.backlog
        ));
        assert!(room.registry.lock().unwrap().is_empty());
        let (out, _frames) = unbounded_channel();
        assert_eq!(room.handle.attach(1, Role::Editor, out), Err(RoomGone));
    }
}
