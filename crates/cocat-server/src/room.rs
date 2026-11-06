//! One thread per open room. It owns the `Doc` and the room file; sockets reach it
//! through channels, because `Doc` and `Update` are not `Send`.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use cocat_core::role::Role;
use cocat_core::store::{FileRoomStore, StoreError, read_updates, room_file};
use thiserror::Error;
use tokio::sync::mpsc::{self as tokio_mpsc, error::TrySendError};
use tokio::sync::oneshot;
use uuid::Uuid;
use yrs::sync::{Message, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, ReadTxn, StateVector, Transact, Update};

pub type ClientId = u64;

/// The durability policy. Every update is written to the room file before it is broadcast,
/// so a process crash loses nothing a client has seen; this timer fsyncs what was written,
/// so a power loss loses at most one second of acknowledged edits. Unload, delete, flush
/// and shutdown fsync as well.
const SYNC_INTERVAL: Duration = Duration::from_secs(1);

/// How many frames a client may fall behind before the room closes it: 1024 frames is
/// roughly twenty seconds of a room at fifty edits per second.
pub const OUTBOUND_FRAMES: usize = 1024;

/// A room file's size times this estimates what its loaded `Doc` costs: the gate's 37.8 MB
/// room measured ~566 MB resident under the allocator the server uses.
pub const MEMORY_PER_FILE_BYTE: u64 = 16;

/// Why the room closed a socket. `1008` is a policy violation, `1013` means try again
/// later, `1011` is the server failing on its side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    Revoked,
    TooSlow,
    OverBudget,
    Failed,
}

impl CloseReason {
    pub fn code(self) -> u16 {
        match self {
            Self::Revoked => 1008,
            Self::TooSlow | Self::OverBudget => 1013,
            Self::Failed => 1011,
        }
    }

    pub fn reason(self) -> &'static str {
        match self {
            Self::Revoked => "access revoked",
            Self::TooSlow => "client too slow",
            Self::OverBudget => "server memory budget reached",
            Self::Failed => "the room could not be loaded",
        }
    }
}

/// The room's end of one client's queue, and the signal that closes that client.
#[derive(Debug)]
pub struct Outbox {
    pub frames: tokio_mpsc::Sender<Bytes>,
    pub close: oneshot::Sender<CloseReason>,
}

#[derive(Debug)]
pub struct Inbox {
    pub frames: tokio_mpsc::Receiver<Bytes>,
    pub close: oneshot::Receiver<CloseReason>,
}

pub fn outbox() -> (Outbox, Inbox) {
    let (frames, inbox_frames) = tokio_mpsc::channel(OUTBOUND_FRAMES);
    let (close, inbox_close) = oneshot::channel();
    (
        Outbox { frames, close },
        Inbox {
            frames: inbox_frames,
            close: inbox_close,
        },
    )
}

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
    #[error("room {room_id} needs {needed} bytes of memory, over the {budget} byte budget")]
    OverBudget {
        room_id: String,
        needed: u64,
        budget: u64,
    },
    #[error("the room did not answer the read")]
    Unanswered,
}

/// Opens rooms on demand and unloads them when their last client leaves.
#[derive(Debug, Clone)]
pub struct RoomManager {
    update_dir: PathBuf,
    rooms: Rooms,
    budget: Option<u64>,
    /// What the cold reads in flight are charged, so a burst of them cannot slip past the
    /// budget between two opens.
    cold_bytes: Arc<AtomicU64>,
}

impl RoomManager {
    pub fn new(update_dir: PathBuf, budget: Option<u64>) -> Self {
        Self {
            update_dir,
            rooms: Arc::default(),
            budget,
            cold_bytes: Arc::default(),
        }
    }

    pub fn get_or_open(&self, room_id: &str) -> Result<RoomHandle, RoomError> {
        let mut rooms = self.rooms.lock().map_err(|_| RoomError::Registry)?;
        if let Some(handle) = rooms.get(room_id) {
            return Ok(handle.clone());
        }
        // The estimate is the file's size, measured rather than parsed: loading the file is
        // about two seconds for the 100k-event room, and holding the registry lock that long
        // would stall every other room. The room thread does the load.
        let estimate = fs::metadata(room_file(&self.update_dir, room_id))
            .map(|metadata| metadata.len())
            .unwrap_or(0)
            .saturating_mul(MEMORY_PER_FILE_BYTE);
        // simplify: an over-budget open is refused rather than evicting an older room; in
        // phase 1 a room is open only while it has clients, so there is nothing to evict.
        // Add LRU unloading when phase 2's REST reads keep rooms open.
        if let Some(budget) = self.budget {
            let cold = self.cold_bytes.load(Ordering::Relaxed);
            let needed = rooms
                .values()
                .fold(estimate.saturating_add(cold), |needed, handle| {
                    needed.saturating_add(handle.estimate)
                });
            if needed > budget {
                return Err(RoomError::OverBudget {
                    room_id: room_id.to_owned(),
                    needed,
                    budget,
                });
            }
        }
        let (commands, receiver) = mpsc::channel();
        let registry = Arc::clone(&self.rooms);
        let task_room_id = room_id.to_owned();
        let update_dir = self.update_dir.clone();
        thread::Builder::new()
            .name(format!("room-{room_id}"))
            .spawn(move || load_room(task_room_id, update_dir, receiver, registry))
            .map_err(RoomError::Spawn)?;
        let handle = RoomHandle { commands, estimate };
        rooms.insert(room_id.to_owned(), handle.clone());
        Ok(handle)
    }

    /// Closes every socket this user has in the room, whatever their client ids. A room
    /// that is not open holds no sockets, so revoking there is a no-op.
    pub async fn revoke(&self, room_id: &str, user: Uuid) {
        let handle = match self.rooms.lock() {
            Ok(rooms) => rooms.get(room_id).cloned(),
            Err(_) => {
                tracing::error!("the room registry is poisoned");
                return;
            }
        };
        let Some(handle) = handle else {
            return;
        };
        if handle.revoke(user).await.is_err() {
            tracing::debug!(room = %room_id, "the room unloaded before the revoke");
        }
    }

    /// Deletes the room and its file, whether or not it is open. An open room unloads the
    /// same way a last detach does, except that it writes nothing more.
    pub async fn delete(&self, room_id: &str) {
        // The room can unload between the lookup and the command, so the handle reports
        // that and the next turn finds either the room that replaced it or the bare file.
        loop {
            let handle = match self.rooms.lock() {
                Ok(rooms) => rooms.get(room_id).cloned(),
                Err(_) => {
                    tracing::error!("the room registry is poisoned");
                    return;
                }
            };
            match handle {
                Some(handle) => {
                    if handle.delete().await.is_ok() {
                        return;
                    }
                }
                None if self.delete_file_if_closed(room_id) => return,
                None => {}
            }
        }
    }

    /// Deletes the file under the registry lock, so a `get_or_open` racing with the delete
    /// cannot load the file the delete is about to remove. False when a room opened first.
    fn delete_file_if_closed(&self, room_id: &str) -> bool {
        match self.rooms.lock() {
            Ok(rooms) if !rooms.contains_key(room_id) => {
                delete_file(&self.update_dir, room_id);
                true
            }
            Ok(_) => false,
            Err(_) => {
                tracing::error!("the room registry is poisoned");
                true
            }
        }
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

    /// Answers one question about a room without keeping it open. An open room answers from
    /// its thread, so the answer includes edits not yet written; a closed room is loaded for
    /// the length of the read and dropped again.
    pub async fn read<T, F>(&self, room_id: &str, read: F) -> Result<T, RoomError>
    where
        T: Send + 'static,
        F: FnOnce(&Doc) -> T + Send + 'static,
    {
        let reader = Arc::new(Mutex::new(Some(read)));
        let open = match self.rooms.lock() {
            Ok(rooms) => rooms.get(room_id).cloned(),
            Err(_) => return Err(RoomError::Registry),
        };
        if let Some(handle) = open
            && let Some(result) = in_room(&handle, Arc::clone(&reader)).await
        {
            return result;
        }
        self.read_file(room_id, reader).await
    }

    async fn read_file<T, F>(
        &self,
        room_id: &str,
        reader: Arc<Mutex<Option<F>>>,
    ) -> Result<T, RoomError>
    where
        T: Send + 'static,
        F: FnOnce(&Doc) -> T + Send + 'static,
    {
        let charged = self.admit_cold_read(room_id)?;
        let update_dir = self.update_dir.clone();
        let id = room_id.to_owned();
        let loaded = tokio::task::spawn_blocking(move || {
            let _charged = charged;
            let doc = Doc::new();
            for update in read_updates(&update_dir, &id)? {
                apply(&doc, &update);
            }
            let read = reader.lock().ok().and_then(|mut slot| slot.take());
            Ok::<_, StoreError>(read.map(|read| read(&doc)))
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, "a cold read panicked");
            RoomError::Unanswered
        })??;
        loaded.ok_or(RoomError::Unanswered)
    }

    /// Admits one cold read and charges it in one step, under the registry lock `get_or_open`
    /// also takes: measuring the budget, then taking it, would let two reads both see the
    /// room for one and both be admitted.
    fn admit_cold_read(&self, room_id: &str) -> Result<Charge, RoomError> {
        let rooms = self.rooms.lock().map_err(|_| RoomError::Registry)?;
        let estimate = fs::metadata(room_file(&self.update_dir, room_id))
            .map(|metadata| metadata.len())
            .unwrap_or(0)
            .saturating_mul(MEMORY_PER_FILE_BYTE);
        if let Some(budget) = self.budget {
            let cold = self.cold_bytes.load(Ordering::Relaxed);
            let needed = rooms
                .values()
                .fold(cold.saturating_add(estimate), |needed, handle| {
                    needed.saturating_add(handle.estimate)
                });
            if needed > budget {
                return Err(RoomError::OverBudget {
                    room_id: room_id.to_owned(),
                    needed,
                    budget,
                });
            }
        }
        self.cold_bytes.fetch_add(estimate, Ordering::Relaxed);
        Ok(Charge {
            bytes: Arc::clone(&self.cold_bytes),
            estimate,
        })
    }
}

/// Releases a cold read's share of the budget however the read ends, a panic included.
struct Charge {
    bytes: Arc<AtomicU64>,
    estimate: u64,
}

impl Drop for Charge {
    fn drop(&mut self) {
        self.bytes.fetch_sub(self.estimate, Ordering::Relaxed);
    }
}

/// Asks an open room for the answer. `None` when the room unloaded first, which leaves the
/// question in the shared slot for the file path to ask.
async fn in_room<T, F>(
    handle: &RoomHandle,
    reader: Arc<Mutex<Option<F>>>,
) -> Option<Result<T, RoomError>>
where
    T: Send + 'static,
    F: FnOnce(&Doc) -> T + Send + 'static,
{
    let (done, answer) = oneshot::channel();
    let shared = Arc::clone(&reader);
    handle
        .send(RoomCommand::Read {
            read: Box::new(move |doc| {
                let read = shared.lock().ok().and_then(|mut slot| slot.take());
                if let Some(read) = read {
                    let _ = done.send(read(doc));
                }
            }),
        })
        .ok()?;
    answer.await.ok().map(Ok)
}

#[derive(Debug, Clone)]
pub struct RoomHandle {
    commands: Sender<RoomCommand>,
    /// What this room is charged against the budget, fixed when it opened.
    estimate: u64,
}

impl RoomHandle {
    pub fn attach(
        &self,
        client: ClientId,
        user: Uuid,
        role: Role,
        out: Outbox,
    ) -> Result<(), RoomGone> {
        self.commands
            .send(RoomCommand::Attach {
                client,
                user,
                role,
                out,
            })
            .map_err(|_| RoomGone)
    }

    pub fn frame(&self, client: ClientId, frame: Bytes) {
        let _ = self.commands.send(RoomCommand::Frame { client, frame });
    }

    /// Waits for the room thread to close every socket this user has here.
    pub async fn revoke(&self, user: Uuid) -> Result<(), RoomGone> {
        let (done, processed) = oneshot::channel();
        self.commands
            .send(RoomCommand::Revoke { user, done })
            .map_err(|_| RoomGone)?;
        let _ = processed.await;
        Ok(())
    }

    /// Waits for the room thread to close every socket, unload and delete the room file.
    pub async fn delete(&self) -> Result<(), RoomGone> {
        let (done, deleted) = oneshot::channel();
        self.commands
            .send(RoomCommand::Delete { done })
            .map_err(|_| RoomGone)?;
        let _ = deleted.await;
        Ok(())
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

    fn send(&self, command: RoomCommand) -> Result<(), RoomGone> {
        self.commands.send(command).map_err(|_| RoomGone)
    }
}

enum RoomCommand {
    Attach {
        client: ClientId,
        user: Uuid,
        role: Role,
        out: Outbox,
    },
    Frame {
        client: ClientId,
        frame: Bytes,
    },
    Detach {
        client: ClientId,
        done: oneshot::Sender<()>,
    },
    Revoke {
        user: Uuid,
        done: oneshot::Sender<()>,
    },
    Delete {
        done: oneshot::Sender<()>,
    },
    Flush {
        done: oneshot::Sender<()>,
    },
    /// Runs a question against the document on the room's own thread.
    Read {
        read: Box<dyn FnOnce(&Doc) + Send>,
    },
}

struct Client {
    user: Uuid,
    role: Role,
    out: Outbox,
}

/// Runs on the room's own thread: loads the file, then serves it. Commands that arrive
/// while the file loads wait in the channel, so a delete queued during a two-second load
/// still removes the file once the load finishes.
fn load_room(
    room_id: String,
    update_dir: PathBuf,
    commands: Receiver<RoomCommand>,
    registry: Rooms,
) {
    match FileRoomStore::open(&update_dir, &room_id) {
        Ok((store, updates)) => room_loop(room_id, update_dir, store, updates, commands, registry),
        Err(error) => {
            tracing::error!(room = %room_id, %error, "the room file cannot be loaded");
            abandon_room(&room_id, &registry, commands);
        }
    }
}

/// Removes a room whose file never loaded. The registry entry goes first, so a retry opens
/// a fresh room, then the receiver is dropped, so later attaches fail. Queued commands are
/// dropped rather than acked: their callers must fall back (`read` reads the file itself,
/// `delete` unlinks it) instead of believing the room did anything.
fn abandon_room(room_id: &str, registry: &Rooms, commands: Receiver<RoomCommand>) {
    match registry.lock() {
        Ok(mut rooms) => {
            rooms.remove(room_id);
            refuse(commands);
        }
        Err(_) => {
            tracing::error!(room = %room_id, "the room registry is poisoned");
            refuse(commands);
        }
    }
}

/// Tells the queued attaches that the room never loaded, and drops everything else.
fn refuse(commands: Receiver<RoomCommand>) {
    while let Ok(command) = commands.try_recv() {
        if let RoomCommand::Attach { out, .. } = command {
            let _ = out.close.send(CloseReason::Failed);
        }
    }
}

fn room_loop(
    room_id: String,
    update_dir: PathBuf,
    mut store: FileRoomStore,
    updates: Vec<Bytes>,
    commands: Receiver<RoomCommand>,
    registry: Rooms,
) {
    let doc = Doc::new();
    let mut clients = HashMap::new();
    let mut backlog = VecDeque::new();
    let mut full_state = None;
    let mut commands = Some(commands);
    for update in updates {
        apply(&doc, &update);
    }

    'serving: loop {
        let mut schedule = SyncSchedule::new(Instant::now());
        let unloaded = loop {
            let command =
                match next_command(&commands, &mut backlog, schedule.remaining(Instant::now())) {
                    Next::Command(command) => command,
                    Next::Idle => {
                        sync(&mut store, &room_id);
                        schedule.advance(Instant::now());
                        continue;
                    }
                    Next::Closed => break Unload::Detached(None),
                };
            let mut broadcast = None;
            match command {
                RoomCommand::Attach {
                    client,
                    user,
                    role,
                    out,
                } => {
                    attach(&doc, &mut clients, client, Client { user, role, out });
                }
                RoomCommand::Frame { client, frame } => {
                    broadcast = handle_frame(
                        &doc,
                        &mut store,
                        &mut full_state,
                        &mut clients,
                        client,
                        frame,
                    );
                }
                RoomCommand::Detach { client, done } => {
                    clients.remove(&client);
                    if clients.is_empty() {
                        break Unload::Detached(Some(done));
                    }
                    let _ = done.send(());
                }
                RoomCommand::Revoke { user, done } => {
                    close_user(&mut clients, user, CloseReason::Revoked);
                    let _ = done.send(());
                }
                RoomCommand::Delete { done } => {
                    close_all(&mut clients, CloseReason::Revoked);
                    break Unload::Deleted(done);
                }
                RoomCommand::Flush { done } => {
                    flush(&mut store, &room_id);
                    let _ = done.send(());
                }
                RoomCommand::Read { read } => read(&doc),
            }
            drain(&commands, &mut backlog);
            // Written before the broadcast so a client that sees an update can read it
            // back from the file; the fsync follows on the timer.
            if backlog.is_empty() {
                write(&mut store, &room_id);
            }
            if let Some(frame) = broadcast {
                broadcast_to(&mut clients, frame);
            }
        };

        match unloaded {
            Unload::Detached(done) => {
                flush(&mut store, &room_id);
                let keep_serving = close_room(&room_id, &registry, &mut commands, &mut backlog);
                if let Some(done) = done {
                    let _ = done.send(());
                }
                if !keep_serving {
                    break 'serving;
                }
            }
            Unload::Deleted(done) => {
                remove_room(
                    &room_id,
                    &update_dir,
                    store,
                    &registry,
                    &mut commands,
                    &mut backlog,
                );
                let _ = done.send(());
                break 'serving;
            }
        }
    }
}

/// Why the room thread stopped serving.
enum Unload {
    /// The last client left, or every handle to the room is gone: flush and unload.
    Detached(Option<oneshot::Sender<()>>),
    /// The room is being deleted: no final write, and the file goes too.
    Deleted(oneshot::Sender<()>),
}

/// When the room thread should fsync next. It only arms while records are unsynced.
struct SyncSchedule {
    next: Instant,
}

impl SyncSchedule {
    fn new(now: Instant) -> Self {
        Self {
            next: now + SYNC_INTERVAL,
        }
    }

    fn remaining(&self, now: Instant) -> Duration {
        self.next.saturating_duration_since(now)
    }

    fn advance(&mut self, now: Instant) {
        self.next = now + SYNC_INTERVAL;
    }
}

enum Next {
    Command(RoomCommand),
    Idle,
    Closed,
}

fn next_command(
    commands: &Option<Receiver<RoomCommand>>,
    backlog: &mut VecDeque<RoomCommand>,
    timeout: Duration,
) -> Next {
    if let Some(command) = backlog.pop_front() {
        return Next::Command(command);
    }
    match commands {
        Some(receiver) => match receiver.recv_timeout(timeout) {
            Ok(command) => Next::Command(command),
            Err(RecvTimeoutError::Timeout) => Next::Idle,
            Err(RecvTimeoutError::Disconnected) => Next::Closed,
        },
        None => Next::Closed,
    }
}

fn drain(commands: &Option<Receiver<RoomCommand>>, backlog: &mut VecDeque<RoomCommand>) {
    if let Some(receiver) = commands {
        while let Ok(command) = receiver.try_recv() {
            backlog.push_back(command);
        }
    }
}

/// Returns true when a client is still queued under the registry lock, so the room must
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
            if backlog
                .iter()
                .any(|command| matches!(command, RoomCommand::Attach { .. }))
            {
                return true;
            }
            rooms.remove(room_id);
            // Dropping the receiver under the registry lock turns every later attach into
            // `RoomGone`, so a handle from `get_or_open` cannot target a dying room.
            commands.take();
            settle(backlog);
            false
        }
        Err(_) => {
            tracing::error!(room = %room_id, "the room registry is poisoned");
            false
        }
    }
}

/// Removes a room that is being deleted, file included, while still holding the registry
/// lock: a `get_or_open` in that window would otherwise load the old file and then lose it
/// under its feet. Unlike `close_room`, a queued attach does not keep the room alive: the
/// file is already gone, and that client learns its role is gone when its bridge re-reads it.
fn remove_room(
    room_id: &str,
    update_dir: &Path,
    store: FileRoomStore,
    registry: &Rooms,
    commands: &mut Option<Receiver<RoomCommand>>,
    backlog: &mut VecDeque<RoomCommand>,
) {
    let mut rooms = match registry.lock() {
        Ok(rooms) => rooms,
        Err(_) => {
            tracing::error!(room = %room_id, "the room registry is poisoned");
            drop(store);
            return delete_file(update_dir, room_id);
        }
    };
    rooms.remove(room_id);
    drop(store);
    delete_file(update_dir, room_id);
    commands.take();
    settle(backlog);
}

fn delete_file(update_dir: &Path, room_id: &str) {
    let path = update_dir.join(format!("{room_id}.y"));
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(room = %room_id, path = %path.display(), %error, "cannot delete the room file");
        }
    }
}

/// Acks the commands a closing room will not process, so their callers do not wait for a
/// room that is already gone (the file was flushed just before `close_room`).
fn settle(backlog: &mut VecDeque<RoomCommand>) {
    for command in backlog.drain(..) {
        match command {
            RoomCommand::Flush { done }
            | RoomCommand::Detach { done, .. }
            | RoomCommand::Revoke { done, .. }
            | RoomCommand::Delete { done } => {
                let _ = done.send(());
            }
            RoomCommand::Attach { .. } | RoomCommand::Frame { .. } | RoomCommand::Read { .. } => {}
        }
    }
}

fn write(store: &mut FileRoomStore, room_id: &str) {
    if let Err(error) = store.write() {
        tracing::warn!(room = %room_id, %error, "cannot write the room");
    }
}

fn sync(store: &mut FileRoomStore, room_id: &str) {
    if !store.needs_sync() {
        return;
    }
    if let Err(error) = store.sync() {
        tracing::warn!(room = %room_id, %error, "cannot sync the room");
    }
}

fn flush(store: &mut FileRoomStore, room_id: &str) {
    if let Err(error) = store.flush() {
        tracing::warn!(room = %room_id, %error, "cannot flush the room");
    }
}

enum Post {
    Sent,
    Full,
    Closed,
}

fn post(out: &Outbox, frame: Bytes) -> Post {
    match out.frames.try_send(frame) {
        Ok(()) => Post::Sent,
        Err(TrySendError::Full(_)) => Post::Full,
        Err(TrySendError::Closed(_)) => Post::Closed,
    }
}

/// Sends one frame to one client. A client that is too far behind, or whose socket is
/// gone, leaves the room instead of buffering without bound.
fn deliver(clients: &mut HashMap<ClientId, Client>, client: ClientId, frame: Bytes) {
    let Some(entry) = clients.get(&client) else {
        return;
    };
    match post(&entry.out, frame) {
        Post::Sent => {}
        Post::Full => drop_client(clients, client, Some(CloseReason::TooSlow)),
        Post::Closed => drop_client(clients, client, None),
    }
}

/// Removes one client; `Some` tells the bridge why it lost the room.
fn drop_client(
    clients: &mut HashMap<ClientId, Client>,
    client: ClientId,
    reason: Option<CloseReason>,
) {
    let Some(entry) = clients.remove(&client) else {
        return;
    };
    if let Some(reason) = reason {
        let _ = entry.out.close.send(reason);
    }
}

/// Closes every socket one user has in this room; the other clients keep receiving.
fn close_user(clients: &mut HashMap<ClientId, Client>, user: Uuid, reason: CloseReason) {
    let doomed: Vec<ClientId> = clients
        .iter()
        .filter(|(_, entry)| entry.user == user)
        .map(|(client, _)| *client)
        .collect();
    for client in doomed {
        drop_client(clients, client, Some(reason));
    }
}

/// Closes every socket in a room that is going away.
fn close_all(clients: &mut HashMap<ClientId, Client>, reason: CloseReason) {
    for (_, entry) in clients.drain() {
        let _ = entry.out.close.send(reason);
    }
}

fn broadcast_to(clients: &mut HashMap<ClientId, Client>, frame: Bytes) {
    let mut dropped = Vec::new();
    for (client, entry) in clients.iter() {
        match post(&entry.out, frame.clone()) {
            Post::Sent => {}
            Post::Full => dropped.push((*client, Some(CloseReason::TooSlow))),
            Post::Closed => dropped.push((*client, None)),
        }
    }
    for (client, reason) in dropped {
        drop_client(clients, client, reason);
    }
}

fn attach(doc: &Doc, clients: &mut HashMap<ClientId, Client>, client: ClientId, entry: Client) {
    let step = Message::Sync(SyncMessage::SyncStep1(doc.transact().state_vector())).encode_v1();
    match post(&entry.out, Bytes::from(step)) {
        Post::Sent => {
            clients.insert(client, entry);
        }
        Post::Full => {
            let _ = entry.out.close.send(CloseReason::TooSlow);
        }
        Post::Closed => {}
    }
}

fn handle_frame(
    doc: &Doc,
    store: &mut FileRoomStore,
    full_state: &mut Option<Bytes>,
    clients: &mut HashMap<ClientId, Client>,
    client: ClientId,
    frame: Bytes,
) -> Option<Bytes> {
    if frame.len() < 2 {
        return None;
    }
    let role = clients.get(&client)?.role;
    // yrs asserts inside its decoders on values it cannot represent (a state vector naming a
    // client id above its mask, for one), so a decoder panic is another undecodable frame
    // rather than the end of the room thread.
    let message =
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Message::decode_v1(&frame)))
        {
            Ok(Ok(message)) => message,
            Ok(Err(error)) => {
                tracing::warn!(%client, %error, "ignoring an undecodable frame");
                return None;
            }
            Err(_) => {
                tracing::warn!(%client, "ignoring a frame the decoder cannot represent");
                return None;
            }
        };
    let Message::Sync(message) = message else {
        return None;
    };
    match message {
        SyncMessage::SyncStep1(state_vector) => {
            let reply = if state_vector.is_empty() {
                full_state_frame(doc, full_state).clone()
            } else {
                Bytes::from(
                    Message::Sync(SyncMessage::SyncStep2(
                        doc.transact().encode_state_as_update_v1(&state_vector),
                    ))
                    .encode_v1(),
                )
            };
            deliver(clients, client, reply);
            None
        }
        SyncMessage::SyncStep2(update) | SyncMessage::Update(update) => {
            if !role.can_write() {
                return None;
            }
            let broadcast = handle_update(doc, store, update);
            if broadcast.is_some() {
                *full_state = None;
            }
            broadcast
        }
    }
}

/// Every cold client asks for the same whole-room state, and a 100k-event room answers in
/// one ~38 MB frame: fifty joining at once would otherwise hold fifty copies in the room's
/// outbound queues. Encoded once, shared by cheap `Bytes` clones, dropped by any update.
fn full_state_frame<'a>(doc: &Doc, cache: &'a mut Option<Bytes>) -> &'a Bytes {
    cache.get_or_insert_with(|| {
        Bytes::from(
            Message::Sync(SyncMessage::SyncStep2(
                doc.transact()
                    .encode_state_as_update_v1(&StateVector::default()),
            ))
            .encode_v1(),
        )
    })
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
    let update =
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Update::decode_v1(payload)))
        {
            Ok(Ok(update)) => update,
            Ok(Err(error)) => {
                tracing::warn!(%error, "skipping a record that is not a v1 update");
                return false;
            }
            Err(_) => {
                tracing::warn!("skipping a record the decoder cannot represent");
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
    use std::fs;
    use std::time::{Duration, Instant};
    use tokio::time::{sleep, timeout};
    use yrs::{Map, ReadTxn, StateVector, Transact};

    use cocat_core::store::sync_count;

    const WAIT: Duration = Duration::from_secs(10);

    fn update_with(key: &str, value: &str) -> Vec<u8> {
        let doc = Doc::new();
        doc.get_or_insert_map("content")
            .insert(&mut doc.transact_mut(), key, value);
        doc.transact()
            .encode_state_as_update_v1(&StateVector::default())
    }

    struct DyingRoom {
        handle: RoomHandle,
        registry: Arc<Mutex<HashMap<String, RoomHandle>>>,
        commands: Option<Receiver<RoomCommand>>,
        backlog: VecDeque<RoomCommand>,
    }

    fn dying_room() -> DyingRoom {
        let (commands, receiver) = mpsc::channel();
        let handle = RoomHandle {
            commands,
            estimate: 0,
        };
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
    fn a_frame_that_breaks_the_decoder_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _records) = FileRoomStore::open(dir.path(), "lab").unwrap();
        let doc = Doc::new();
        let mut clients = HashMap::new();
        let (out, _inbox) = outbox();
        clients.insert(
            7,
            Client {
                user: Uuid::new_v4(),
                role: Role::Viewer,
                out,
            },
        );

        let vector = [
            0x01, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01, 0,
        ];
        let mut hostile = vec![0u8, 0u8, vector.len() as u8];
        hostile.extend_from_slice(&vector);

        assert!(
            handle_frame(
                &doc,
                &mut store,
                &mut None,
                &mut clients,
                7,
                Bytes::from(hostile)
            )
            .is_none()
        );
    }

    #[test]
    fn a_queued_attach_keeps_a_dying_room_alive() {
        let mut room = dying_room();
        let (out, _inbox) = outbox();
        room.handle
            .attach(1, Uuid::new_v4(), Role::Editor, out)
            .unwrap();

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
        let (out, _inbox) = outbox();
        assert_eq!(
            room.handle.attach(1, Uuid::new_v4(), Role::Editor, out),
            Err(RoomGone)
        );
    }

    #[test]
    fn a_queued_flush_does_not_keep_a_dying_room_open() {
        let mut room = dying_room();
        let (done, mut flushed) = oneshot::channel();
        room.handle
            .commands
            .send(RoomCommand::Flush { done })
            .unwrap();

        assert!(!close_room(
            "room",
            &room.registry,
            &mut room.commands,
            &mut room.backlog
        ));
        assert!(room.registry.lock().unwrap().is_empty());
        assert!(flushed.try_recv().is_ok());
    }

    #[tokio::test]
    async fn a_quiet_room_syncs_within_a_second() {
        let dir = tempfile::tempdir().unwrap();
        let manager = RoomManager::new(dir.path().to_owned(), None);
        let room = manager.get_or_open("room-quiet").unwrap();
        let (out, mut inbox) = outbox();
        room.attach(1, Uuid::new_v4(), Role::Editor, out).unwrap();
        timeout(WAIT, inbox.frames.recv()).await.unwrap().unwrap();

        let payload = update_with("key", "value");
        room.frame(
            1,
            Bytes::from(Message::Sync(SyncMessage::Update(payload)).encode_v1()),
        );
        timeout(WAIT, inbox.frames.recv()).await.unwrap().unwrap();
        let written = sync_count();

        let deadline = Instant::now() + Duration::from_secs(2);
        while sync_count() == written && Instant::now() < deadline {
            sleep(Duration::from_millis(20)).await;
        }
        assert!(sync_count() > written, "the quiet room never synced");
        let bytes = fs::read(dir.path().join("room-quiet.y")).unwrap();
        assert!(bytes.len() > cocat_core::store::HEADER.len());
        room.detach(1).await;
    }

    async fn join(room: &RoomHandle, client: ClientId, out: Outbox, inbox: &mut Inbox) -> Bytes {
        room.attach(client, Uuid::new_v4(), Role::Editor, out)
            .unwrap();
        inbox.frames.recv().await.unwrap();
        room.frame(
            client,
            Bytes::from(Message::Sync(SyncMessage::SyncStep1(StateVector::default())).encode_v1()),
        );
        timeout(WAIT, inbox.frames.recv()).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn cold_joins_share_one_encoded_full_state() {
        let dir = tempfile::tempdir().unwrap();
        let manager = RoomManager::new(dir.path().to_owned(), None);
        let room = manager.get_or_open("room-share").unwrap();
        let (out_a, mut inbox_a) = outbox();
        let (out_b, mut inbox_b) = outbox();

        let first = join(&room, 1, out_a, &mut inbox_a).await;
        let second = join(&room, 2, out_b, &mut inbox_b).await;

        assert_eq!(first, second);
        assert_eq!(
            first.as_ptr(),
            second.as_ptr(),
            "the full state was encoded one more time for the second client"
        );
        room.detach(1).await;
        room.detach(2).await;
    }

    #[tokio::test]
    async fn an_update_between_joins_invalidates_the_full_state() {
        let dir = tempfile::tempdir().unwrap();
        let manager = RoomManager::new(dir.path().to_owned(), None);
        let room = manager.get_or_open("room-invalidate").unwrap();
        let (out_a, mut inbox_a) = outbox();
        let (out_b, mut inbox_b) = outbox();

        let first = join(&room, 1, out_a, &mut inbox_a).await;
        room.frame(
            1,
            Bytes::from(Message::Sync(SyncMessage::Update(update_with("key", "two"))).encode_v1()),
        );
        inbox_a.frames.recv().await.unwrap();

        let second = join(&room, 2, out_b, &mut inbox_b).await;

        assert_ne!(first, second);
        assert_ne!(first.as_ptr(), second.as_ptr());
        let doc = Doc::new();
        match Message::decode_v1(&second).unwrap() {
            Message::Sync(SyncMessage::SyncStep2(payload)) => {
                doc.transact_mut()
                    .apply_update(Update::decode_v1(&payload).unwrap())
                    .unwrap();
            }
            _ => panic!("a join must be answered with the state as an update"),
        }
        let content = doc.get_or_insert_map("content");
        let txn = doc.transact();
        assert_eq!(
            content.get(&txn, "key").map(|value| value.to_string(&txn)),
            Some("two".to_owned())
        );
        room.detach(1).await;
        room.detach(2).await;
    }
}
