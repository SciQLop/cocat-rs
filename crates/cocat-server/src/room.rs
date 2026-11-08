//! One thread per open room. It owns the `Doc` and the room file; sockets reach it
//! through channels, because `Doc` and `Update` are not `Send`.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use cocat_core::model;
use cocat_core::role::Role;
use cocat_core::store::{FileRoomStore, StoreError, read_updates, room_file};
use sqlx::SqlitePool;
use thiserror::Error;
use tokio::sync::mpsc::{self as tokio_mpsc, error::TrySendError};
use tokio::sync::{Semaphore, oneshot};
use uuid::Uuid;
use yrs::sync::{AwarenessUpdate, Message, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{ClientID, Doc, ReadTxn, StateVector, Transact, Update};

use crate::history::stamps::{
    ENTRY, Stamp, StampFile, read_stamps, stamp_for, stamps_file, valid_stamps,
};
use crate::presence::Presence;

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

/// Two cold reads at once: each one parses a whole room file into a fresh document, and
/// SciQLop syncs over the websocket, so these reads are rare and do not need one per core.
pub const COLD_READ_SLOTS: usize = 2;

/// One megabyte, the unit administrators set quotas in.
const MEGABYTE: u64 = 1024 * 1024;
/// A room can unload between being found and being written to; a retry opens it again.
const WRITE_TRIES: usize = 3;

fn megabytes(mb: u64) -> u64 {
    mb.saturating_mul(MEGABYTE)
}

/// How large a room file may grow, and how much the files of one owner's rooms may total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageLimits {
    pub room: u64,
    pub owner: u64,
}

impl Default for StorageLimits {
    fn default() -> Self {
        Self {
            room: 1024 * 1024 * 1024,
            owner: 5120 * 1024 * 1024,
        }
    }
}

/// Why the room closed a socket. `1008` is a policy violation, `1013` means try again
/// later, `1011` is the server failing on its side, and `4507` is the room's storage limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    Revoked,
    TooSlow,
    OverBudget,
    TooLarge,
    Full,
    Failed,
}

impl CloseReason {
    pub fn code(self) -> u16 {
        match self {
            Self::Revoked => 1008,
            Self::TooLarge => 1009,
            Self::TooSlow | Self::OverBudget => 1013,
            Self::Full => 4507,
            Self::Failed => 1011,
        }
    }

    pub fn reason(self) -> &'static str {
        match self {
            Self::Revoked => "access revoked",
            Self::TooSlow => "client too slow",
            Self::OverBudget => "server memory budget reached",
            Self::TooLarge => "message too large",
            Self::Full => "room storage limit reached",
            Self::Failed => "the room could not be loaded",
        }
    }

    /// The label the metrics carry; fixed strings only, never a room id.
    pub fn label(self) -> &'static str {
        match self {
            Self::Revoked => "revoked",
            Self::TooSlow => "too_slow",
            Self::OverBudget => "over_budget",
            Self::TooLarge => "too_large",
            Self::Full => "full",
            Self::Failed => "failed",
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
    #[error("the room unloaded before the write reached it")]
    Gone,
    #[error("the room reached its storage limit")]
    Full,
    #[error(transparent)]
    Db(#[from] crate::db::DbError),
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
    /// How many cold reads may parse at once: each one loads a whole room file.
    cold_reads: Arc<Semaphore>,
    /// The room and owner storage limits, applied to the file when a room opens.
    storage: StorageLimits,
}

impl RoomManager {
    pub fn new(update_dir: PathBuf, budget: Option<u64>) -> Self {
        Self::with_cold_reads(update_dir, budget, COLD_READ_SLOTS)
    }

    fn with_cold_reads(update_dir: PathBuf, budget: Option<u64>, cold_reads: usize) -> Self {
        Self {
            update_dir,
            rooms: Arc::default(),
            budget,
            cold_bytes: Arc::default(),
            cold_reads: Arc::new(Semaphore::new(cold_reads.max(1))),
            storage: StorageLimits::default(),
        }
    }

    pub fn with_storage(mut self, storage: StorageLimits) -> Self {
        self.storage = storage;
        self
    }

    pub fn get_or_open(&self, room_id: &str) -> Result<RoomHandle, RoomError> {
        self.get_or_open_within(room_id, self.storage.room.min(self.storage.owner))
    }

    /// Opens a room whose file may reach `allowance` bytes. The caller measures what the
    /// owner's other rooms take; the room itself only enforces the number it was given.
    pub fn get_or_open_within(
        &self,
        room_id: &str,
        allowance: u64,
    ) -> Result<RoomHandle, RoomError> {
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
                crate::telemetry::refused("cold_read_busy");
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
            .spawn(move || load_room(task_room_id, update_dir, allowance, receiver, registry))
            .map_err(RoomError::Spawn)?;
        let handle = RoomHandle { commands, estimate };
        rooms.insert(room_id.to_owned(), handle.clone());
        crate::telemetry::rooms_open(rooms.len());
        Ok(handle)
    }

    /// The bytes a room's file may reach: the room limit, or what its owner's quota leaves
    /// once the owner's other room files are counted.
    ///
    /// simplify: the allowance is measured when the room opens, so two of one owner's rooms
    /// opening together can each spend the remaining quota (overshoot ≤ remaining × open
    /// rooms). Upgrade path: one shared per-owner byte counter in the manager.
    pub async fn allowance(&self, pool: &SqlitePool, room_id: &str) -> Result<u64, RoomError> {
        let room_limit = match crate::db::room_quota(pool, room_id).await? {
            Some(mb) => megabytes(mb),
            None => self.storage.room,
        };
        let Some(owner) = crate::db::room_owner(pool, room_id).await? else {
            return Ok(room_limit);
        };
        let owner_limit = match crate::db::user_quota(pool, owner).await? {
            Some(mb) => megabytes(mb),
            None => self.storage.owner,
        };
        let mut used = 0u64;
        for owned in crate::db::owned_rooms(pool, owner).await? {
            if owned != room_id {
                used = used.saturating_add(self.room_size(&owned));
            }
        }
        Ok(room_limit.min(owner_limit.saturating_sub(used)))
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

    /// The limits the process was configured with, before any per-account override.
    pub fn limits(&self) -> StorageLimits {
        self.storage
    }

    /// Whether the room is open right now, so its newest edits may not have reached the file.
    pub fn is_open(&self, room_id: &str) -> bool {
        self.rooms
            .lock()
            .map(|rooms| rooms.contains_key(room_id))
            .unwrap_or(false)
    }

    /// Sockets open in `room_id` across every client id, or zero when it is not open.
    pub async fn clients_of(&self, room_id: &str) -> u64 {
        let handle = match self.rooms.lock() {
            Ok(rooms) => rooms.get(room_id).cloned(),
            Err(_) => None,
        };
        match handle {
            Some(handle) => handle.clients().await,
            None => 0,
        }
    }

    /// How many rooms are open and how many sockets they hold together.
    pub async fn open_counts(&self) -> (u64, u64) {
        let handles: Vec<RoomHandle> = match self.rooms.lock() {
            Ok(rooms) => rooms.values().cloned().collect(),
            Err(_) => return (0, 0),
        };
        let open = handles.len() as u64;
        let mut clients = 0u64;
        for handle in handles {
            clients = clients.saturating_add(handle.clients().await);
        }
        (open, clients)
    }

    /// What the room takes on disk, its file and its stamps; zero when nothing was written.
    pub fn room_size(&self, room_id: &str) -> u64 {
        let size = |path: PathBuf| fs::metadata(path).map_or(0, |metadata| metadata.len());
        size(room_file(&self.update_dir, room_id))
            .saturating_add(size(stamps_file(&self.update_dir, room_id)))
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
        let started = Instant::now();
        let loaded = self
            .cold(room_id, 1, move |update_dir, id| {
                let doc = Doc::new();
                for update in read_updates(update_dir, id)? {
                    let _ = apply(&doc, &update);
                }
                let read = reader.lock().ok().and_then(|mut slot| slot.take());
                Ok(read.map(|read| read(&doc)))
            })
            .await?;
        crate::telemetry::room_opened(started.elapsed().as_secs_f64());
        loaded.ok_or(RoomError::Unanswered)
    }

    /// Waits for an open room to write its pending records; a closed room has none.
    pub async fn flush(&self, room_id: &str) -> Result<(), RoomError> {
        let open = self
            .rooms
            .lock()
            .map_err(|_| RoomError::Registry)?
            .get(room_id)
            .cloned();
        if let Some(handle) = open {
            handle.flush().await;
        }
        Ok(())
    }

    /// Writes `user`'s restore of `version`, like a client frame: quota-checked, appended,
    /// stamped as a restore and broadcast. A room nobody is in opens for the write and unloads
    /// after it. `Ok(false)` when the update changed nothing.
    pub async fn restore_as(
        &self,
        room_id: &str,
        allowance: u64,
        user: Uuid,
        version: u64,
        update: Vec<u8>,
    ) -> Result<bool, RoomError> {
        for _ in 0..WRITE_TRIES {
            let rooms = self.clone();
            let id = room_id.to_owned();
            let room =
                tokio::task::spawn_blocking(move || rooms.get_or_open_within(&id, allowance))
                    .await
                    .map_err(|_| RoomError::Unanswered)??;
            match room.restore_as(user, version, update.clone()).await {
                Err(RoomError::Gone) => continue,
                written => return written,
            }
        }
        Err(RoomError::Gone)
    }

    /// Answers from the room's records as written on disk, each with its valid stamp, after
    /// the open room flushed: a version just written is part of the answer. The read is
    /// charged for the `docs` whole documents `read` holds at once.
    pub async fn read_log<T, F>(&self, room_id: &str, docs: u64, read: F) -> Result<T, RoomError>
    where
        T: Send + 'static,
        F: FnOnce(&[Bytes], &[Option<Stamp>]) -> T + Send + 'static,
    {
        self.flush(room_id).await?;
        self.cold(room_id, docs, move |update_dir, id| {
            let records = read_updates(update_dir, id)?;
            let stamps = read_stamps(update_dir, id).map_err(|source| StoreError::Io {
                path: stamps_file(update_dir, id),
                source,
            })?;
            let slots = valid_stamps(&records, stamps);
            Ok(read(&records, &slots))
        })
        .await
    }

    /// Runs `load` on the blocking pool under a cold-read slot and the memory budget, charged
    /// for the `docs` documents it builds.
    async fn cold<T, F>(&self, room_id: &str, docs: u64, load: F) -> Result<T, RoomError>
    where
        T: Send + 'static,
        F: FnOnce(&Path, &str) -> Result<T, StoreError> + Send + 'static,
    {
        // The slot moves into the blocking run, so a request dropped mid-parse keeps the room
        // it is loading charged until the parse ends.
        let slot = Arc::clone(&self.cold_reads)
            .acquire_owned()
            .await
            .map_err(|_| RoomError::Unanswered)?;
        let charged = self.admit_cold_read(room_id, docs)?;
        let update_dir = self.update_dir.clone();
        let id = room_id.to_owned();
        let loaded = tokio::task::spawn_blocking(move || {
            let _slot = slot;
            let _charged = charged;
            load(&update_dir, &id)
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, "a cold read panicked");
            RoomError::Unanswered
        })??;
        Ok(loaded)
    }

    /// Admits one cold read and charges it in one step, under the registry lock `get_or_open`
    /// also takes: measuring the budget, then taking it, would let two reads both see the
    /// room for one and both be admitted.
    fn admit_cold_read(&self, room_id: &str, docs: u64) -> Result<Charge, RoomError> {
        let rooms = self.rooms.lock().map_err(|_| RoomError::Registry)?;
        let estimate = fs::metadata(room_file(&self.update_dir, room_id))
            .map(|metadata| metadata.len())
            .unwrap_or(0)
            .saturating_mul(MEMORY_PER_FILE_BYTE)
            .saturating_mul(docs);
        if let Some(budget) = self.budget {
            let cold = self.cold_bytes.load(Ordering::Relaxed);
            let needed = rooms
                .values()
                .fold(cold.saturating_add(estimate), |needed, handle| {
                    needed.saturating_add(handle.estimate)
                });
            if needed > budget {
                crate::telemetry::refused("cold_read_busy");
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
    /// `email` is who the client is to the others in the room's awareness.
    pub fn attach(
        &self,
        client: ClientId,
        user: Uuid,
        email: String,
        role: Role,
        out: Outbox,
    ) -> Result<(), RoomGone> {
        self.commands
            .send(RoomCommand::Attach {
                client,
                user,
                email,
                role,
                out,
            })
            .map_err(|_| RoomGone)
    }

    #[cfg(test)]
    pub fn panic(&self) {
        let _ = self.commands.send(RoomCommand::Panic);
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

    /// Has the room thread take `update` as if `user` had sent it in a frame, stamped as a
    /// restore of `version`.
    pub async fn restore_as(
        &self,
        user: Uuid,
        version: u64,
        update: Vec<u8>,
    ) -> Result<bool, RoomError> {
        let (done, written) = oneshot::channel();
        self.send(RoomCommand::Restore {
            user,
            version,
            update,
            done,
        })
        .map_err(|_| RoomError::Gone)?;
        match written.await {
            Ok(Ok(changed)) => Ok(changed),
            Ok(Err(_)) => Err(RoomError::Full),
            Err(_) => Err(RoomError::Unanswered),
        }
    }

    /// How many sockets the room thread holds open right now.
    pub async fn clients(&self) -> u64 {
        let (done, count) = oneshot::channel();
        if self.commands.send(RoomCommand::Clients { done }).is_ok() {
            count.await.unwrap_or(0)
        } else {
            0
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
        email: String,
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
    /// Answers how many sockets the room still holds.
    Clients {
        done: oneshot::Sender<u64>,
    },
    /// Runs a question against the document on the room's own thread.
    Read {
        read: Box<dyn FnOnce(&Doc) + Send>,
    },
    /// A restore the server writes on a user's behalf; answers whether it changed the room.
    Restore {
        user: Uuid,
        version: u64,
        update: Vec<u8>,
        done: oneshot::Sender<Result<bool, CloseReason>>,
    },
    /// Only the tests send this: the room thread has to survive its own panic.
    #[cfg(test)]
    Panic,
}

struct Client {
    user: Uuid,
    email: String,
    role: Role,
    out: Outbox,
}

/// Runs on the room's own thread: loads the file, then serves it. Commands that arrive
/// while the file loads wait in the channel, so a delete queued during a two-second load
/// still removes the file once the load finishes.
fn load_room(
    room_id: String,
    update_dir: PathBuf,
    allowance: u64,
    commands: Receiver<RoomCommand>,
    registry: Rooms,
) {
    let started = Instant::now();
    let (store, updates) = match FileRoomStore::open(&update_dir, &room_id) {
        Ok(opened) => opened,
        Err(error) => {
            tracing::error!(room = %room_id, %error, "the room file cannot be loaded");
            return abandon_room(&room_id, &registry, commands);
        }
    };
    crate::telemetry::room_opened(started.elapsed().as_secs_f64());
    let doc = replay(&updates);
    // A v2 client would merge its edits into the first layout; the import command migrates.
    if model::is_v1(&doc.transact()) {
        tracing::error!(room = %room_id, "the room file holds the first layout; run the import");
        return abandon_room(&room_id, &registry, commands);
    }
    room_loop(
        room_id, update_dir, allowance, store, doc, commands, registry,
    )
}

/// Removes a room whose file never loaded. The registry entry goes first, so a retry opens
/// a fresh room, then the receiver is dropped, so later attaches fail. Queued commands are
/// dropped rather than acked: their callers must fall back (`read` reads the file itself,
/// `delete` unlinks it) instead of believing the room did anything.
fn abandon_room(room_id: &str, registry: &Rooms, commands: Receiver<RoomCommand>) {
    match registry.lock() {
        Ok(mut rooms) => {
            rooms.remove(room_id);
            crate::telemetry::rooms_open(rooms.len());
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
    limit: u64,
    mut store: FileRoomStore,
    doc: Doc,
    commands: Receiver<RoomCommand>,
    registry: Rooms,
) {
    let mut stamps = open_stamps(&update_dir, &room_id);
    let mut clients = HashMap::new();
    let mut backlog = VecDeque::new();
    let mut full_state = None;
    let mut commands = Some(commands);
    let mut seen = doc.transact().state_vector();
    let mut presence = Presence::default();

    'serving: loop {
        let mut schedule = SyncSchedule::new(Instant::now());
        let unloaded = loop {
            let command =
                match next_command(&commands, &mut backlog, schedule.remaining(Instant::now())) {
                    Next::Command(command) => command,
                    Next::Idle => {
                        sync(&mut store, &mut stamps, &room_id);
                        schedule.advance(Instant::now());
                        continue;
                    }
                    Next::Closed => break Unload::Detached(None),
                };
            let stepped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut broadcast = None;
                let mut answer = None;
                let step = match command {
                    RoomCommand::Attach {
                        client,
                        user,
                        email,
                        role,
                        out,
                    } => {
                        let entry = Client {
                            user,
                            email,
                            role,
                            out,
                        };
                        attach(&doc, &mut clients, client, entry);
                        if let Some(snapshot) = presence.snapshot() {
                            deliver(&mut clients, client, snapshot);
                        }
                        Step::Serving
                    }
                    RoomCommand::Frame { client, frame } => {
                        let mut serving = Serving {
                            doc: &doc,
                            store: &mut store,
                            stamps: &mut stamps,
                            seen: &mut seen,
                            full_state: &mut full_state,
                            clients: &mut clients,
                            presence: &mut presence,
                            limit,
                        };
                        match handle_frame(&mut serving, client, frame) {
                            Ok(frame) => broadcast = frame,
                            Err(reason) => {
                                tracing::warn!(
                                    room = %room_id,
                                    "an update was refused at the room's storage limit"
                                );
                                if let Some(closed) = clients.remove(&client) {
                                    let _ = closed.out.close.send(reason);
                                }
                            }
                        }
                        Step::Serving
                    }
                    RoomCommand::Detach { client, done } => {
                        clients.remove(&client);
                        if clients.is_empty() {
                            Step::Detach(Some(done))
                        } else {
                            let _ = done.send(());
                            Step::Serving
                        }
                    }
                    RoomCommand::Revoke { user, done } => {
                        close_user(&mut clients, user, CloseReason::Revoked);
                        let _ = done.send(());
                        Step::Serving
                    }
                    RoomCommand::Delete { done } => {
                        close_all(&mut clients, CloseReason::Revoked);
                        Step::Delete(done)
                    }
                    RoomCommand::Flush { done } => {
                        let started = Instant::now();
                        flush(&mut store, &mut stamps, &room_id);
                        crate::telemetry::room_flushed(started.elapsed().as_secs_f64());
                        let _ = done.send(());
                        Step::Serving
                    }
                    RoomCommand::Clients { done } => {
                        let _ = done.send(clients.len() as u64);
                        Step::Serving
                    }
                    RoomCommand::Read { read } => {
                        read(&doc);
                        Step::Serving
                    }
                    RoomCommand::Restore {
                        user,
                        version,
                        update,
                        done,
                    } => {
                        let written = handle_update(
                            &doc,
                            &mut store,
                            &mut stamps,
                            &mut seen,
                            limit,
                            update,
                            Author::Restore { user, version },
                        );
                        if let Ok(Some(frame)) = &written {
                            full_state = None;
                            broadcast = Some(frame.clone());
                        }
                        answer = Some((done, written.map(|frame| frame.is_some())));
                        if clients.is_empty() {
                            Step::Detach(None)
                        } else {
                            Step::Serving
                        }
                    }
                    #[cfg(test)]
                    RoomCommand::Panic => panic!("a command asked the room to panic"),
                };
                drain(&commands, &mut backlog);
                // Written before the broadcast so a client that sees an update can read it
                // back from the file; the fsync follows on the timer.
                if backlog.is_empty() {
                    write(&mut store, &mut stamps, &room_id);
                }
                if let Some(frame) = broadcast {
                    broadcast_to(&mut clients, frame);
                }
                if let Some(frame) = presence.depart(|client| clients.contains_key(&client)) {
                    broadcast_to(&mut clients, frame);
                }
                if let Some((done, written)) = answer {
                    let _ = done.send(written);
                }
                step
            }));
            match stepped {
                Ok(Step::Serving) => {}
                Ok(Step::Detach(done)) => break Unload::Detached(done),
                Ok(Step::Delete(done)) => break Unload::Deleted(done),
                Err(_) => {
                    // A room that keeps its registry entry after a panic can never be
                    // reached again, so the whole room is abandoned instead.
                    tracing::error!(room = %room_id, "a room thread panicked; unloading the room");
                    close_all(&mut clients, CloseReason::Failed);
                    drop_queued(&mut commands, &mut backlog, &update_dir, &room_id);
                    remove_entry(&room_id, &registry);
                    break 'serving;
                }
            }
        };

        match unloaded {
            Unload::Detached(done) => {
                flush(&mut store, &mut stamps, &room_id);
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
                    stamps,
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

/// What one command asked of the serving loop.
enum Step {
    Serving,
    Detach(Option<oneshot::Sender<()>>),
    Delete(oneshot::Sender<()>),
}

/// After a panic nobody will serve what is queued, so waiting attachments are told the
/// room failed and a queued delete still removes the file.
fn drop_queued(
    commands: &mut Option<Receiver<RoomCommand>>,
    backlog: &mut VecDeque<RoomCommand>,
    update_dir: &Path,
    room_id: &str,
) {
    let mut waiting: Vec<RoomCommand> = backlog.drain(..).collect();
    if let Some(receiver) = commands.take() {
        waiting.extend(receiver.try_iter());
    }
    for command in waiting {
        match command {
            RoomCommand::Attach { out, .. } => {
                let _ = out.close.send(CloseReason::Failed);
            }
            RoomCommand::Delete { done } => {
                delete_file(update_dir, room_id);
                let _ = done.send(());
            }
            _ => {}
        }
    }
}

fn remove_entry(room_id: &str, registry: &Rooms) {
    if let Ok(mut rooms) = registry.lock() {
        rooms.remove(room_id);
        crate::telemetry::rooms_open(rooms.len());
    }
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

/// Returns true when a client or a write is still queued under the registry lock, so the
/// room must keep serving; false when it removed itself and dropped its command receiver.
fn close_room(
    room_id: &str,
    registry: &Rooms,
    commands: &mut Option<Receiver<RoomCommand>>,
    backlog: &mut VecDeque<RoomCommand>,
) -> bool {
    match registry.lock() {
        Ok(mut rooms) => {
            drain(commands, backlog);
            if backlog.iter().any(|command| {
                matches!(
                    command,
                    RoomCommand::Attach { .. } | RoomCommand::Restore { .. }
                )
            }) {
                return true;
            }
            rooms.remove(room_id);
            crate::telemetry::rooms_open(rooms.len());
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
    stamps: Option<StampFile>,
    registry: &Rooms,
    commands: &mut Option<Receiver<RoomCommand>>,
    backlog: &mut VecDeque<RoomCommand>,
) {
    let mut rooms = match registry.lock() {
        Ok(rooms) => rooms,
        Err(_) => {
            tracing::error!(room = %room_id, "the room registry is poisoned");
            drop((store, stamps));
            return delete_file(update_dir, room_id);
        }
    };
    rooms.remove(room_id);
    crate::telemetry::rooms_open(rooms.len());
    drop((store, stamps));
    delete_file(update_dir, room_id);
    commands.take();
    settle(backlog);
}

/// Removes the room file and its stamps, so a later room with the same id starts clean.
fn delete_file(update_dir: &Path, room_id: &str) {
    for path in [
        room_file(update_dir, room_id),
        stamps_file(update_dir, room_id),
    ] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(room = %room_id, path = %path.display(), %error, "cannot delete a room file");
            }
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
            RoomCommand::Clients { done } => {
                let _ = done.send(0);
            }
            RoomCommand::Attach { .. }
            | RoomCommand::Frame { .. }
            | RoomCommand::Read { .. }
            | RoomCommand::Restore { .. } => {}
            #[cfg(test)]
            RoomCommand::Panic => {}
        }
    }
}

/// The room runs without history rather than not at all when its stamps file cannot be
/// opened (a room id whose file name is already at the 255-byte limit, for one).
fn open_stamps(update_dir: &Path, room_id: &str) -> Option<StampFile> {
    StampFile::open(update_dir, room_id)
        .inspect_err(|error| {
            tracing::warn!(room = %room_id, %error, "cannot open the stamps file, the room runs without history");
        })
        .ok()
}

/// Stamps are written only once their records are, so a failed room write never leaves a
/// stamp ahead of its record on the disk.
fn write(store: &mut FileRoomStore, stamps: &mut Option<StampFile>, room_id: &str) {
    if let Err(error) = store.write() {
        tracing::warn!(room = %room_id, %error, "cannot write the room");
        return;
    }
    if let Some(Err(error)) = stamps.as_mut().map(StampFile::write) {
        tracing::warn!(room = %room_id, %error, "cannot write the room's stamps");
    }
}

fn sync(store: &mut FileRoomStore, stamps: &mut Option<StampFile>, room_id: &str) {
    if store.needs_sync()
        && let Err(error) = store.sync()
    {
        tracing::warn!(room = %room_id, %error, "cannot sync the room");
    }
    if let Some(Err(error)) = stamps.as_mut().map(StampFile::sync) {
        tracing::warn!(room = %room_id, %error, "cannot sync the room's stamps");
    }
}

fn flush(store: &mut FileRoomStore, stamps: &mut Option<StampFile>, room_id: &str) {
    write(store, stamps, room_id);
    sync(store, stamps, room_id);
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
    broadcast_except(clients, None, frame);
}

/// Sends `frame` to every client but `sender`.
fn broadcast_except(
    clients: &mut HashMap<ClientId, Client>,
    sender: Option<ClientId>,
    frame: Bytes,
) {
    let mut dropped = Vec::new();
    for (client, entry) in clients
        .iter()
        .filter(|(client, _)| Some(**client) != sender)
    {
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
    serving: &mut Serving<'_>,
    client: ClientId,
    frame: Bytes,
) -> Result<Option<Bytes>, CloseReason> {
    if frame.len() < 2 || crate::presence::oversized(&frame) {
        return Ok(None);
    }
    let Some((user, role)) = serving
        .clients
        .get(&client)
        .map(|client| (client.user, client.role))
    else {
        return Ok(None);
    };
    // yrs asserts inside its decoders on values it cannot represent (a state vector naming a
    // client id above its mask, for one), so a decoder panic is another undecodable frame
    // rather than the end of the room thread.
    let message =
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Message::decode_v1(&frame)))
        {
            Ok(Ok(message)) => message,
            Ok(Err(error)) => {
                tracing::warn!(%client, %error, "ignoring an undecodable frame");
                return Ok(None);
            }
            Err(_) => {
                tracing::warn!(%client, "ignoring a frame the decoder cannot represent");
                return Ok(None);
            }
        };
    let message = match message {
        Message::Sync(message) => message,
        Message::Awareness(update) => {
            relay_awareness(serving, client, update);
            return Ok(None);
        }
        _ => return Ok(None),
    };
    match message {
        SyncMessage::SyncStep1(state_vector) => {
            let reply = if state_vector.is_empty() {
                full_state_frame(serving.doc, serving.full_state).clone()
            } else {
                Bytes::from(
                    Message::Sync(SyncMessage::SyncStep2(
                        serving
                            .doc
                            .transact()
                            .encode_state_as_update_v1(&state_vector),
                    ))
                    .encode_v1(),
                )
            };
            deliver(serving.clients, client, reply);
            Ok(None)
        }
        SyncMessage::SyncStep2(update) | SyncMessage::Update(update) => {
            if !role.can_write() {
                return Ok(None);
            }
            let broadcast = handle_update(
                serving.doc,
                serving.store,
                serving.stamps,
                serving.seen,
                serving.limit,
                update,
                Author::Client(user),
            )?;
            if broadcast.is_some() {
                *serving.full_state = None;
            }
            Ok(broadcast)
        }
    }
}

/// Every role may publish its presence, a viewer included: it writes nothing to the room.
fn relay_awareness(serving: &mut Serving<'_>, client: ClientId, update: AwarenessUpdate) {
    let Some((user, email)) = serving
        .clients
        .get(&client)
        .map(|entry| (entry.user, entry.email.clone()))
    else {
        return;
    };
    if let Some(frame) = serving.presence.update(client, user, &email, update) {
        broadcast_except(serving.clients, Some(client), frame);
    }
}

/// What one frame may reach on the room's thread: the document, the file it persists to, the
/// room's own state vector, the cached full state, and the clients to answer or close.
struct Serving<'a> {
    doc: &'a Doc,
    store: &'a mut FileRoomStore,
    stamps: &'a mut Option<StampFile>,
    seen: &'a mut StateVector,
    full_state: &'a mut Option<Bytes>,
    clients: &'a mut HashMap<ClientId, Client>,
    presence: &'a mut Presence,
    limit: u64,
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

fn handle_update(
    doc: &Doc,
    store: &mut FileRoomStore,
    stamps: &mut Option<StampFile>,
    seen: &mut StateVector,
    limit: u64,
    payload: Vec<u8>,
    author: Author,
) -> Result<Option<Bytes>, CloseReason> {
    // Python's wire_file drops this payload: an empty v1 update is two zero bytes, and
    // clients send one as their SyncStep2 on every connect.
    if payload == [0, 0] {
        return Ok(None);
    }
    // The payload's size and one stamp bound what an append adds, and the check runs before
    // the update is applied so a refused update leaves the document and the files as they
    // were. A repeated update at the boundary is refused with the rest: it costs a
    // reconnection, not data.
    let (stamped, stamp) = stamps
        .as_ref()
        .map_or((0, 0), |stamps| (stamps.size(), ENTRY as u64));
    let after = store
        .total_size()
        .saturating_add(stamped)
        .saturating_add(stamp)
        .saturating_add(payload.len() as u64);
    if after > limit {
        crate::telemetry::refused("storage_quota");
        return Err(CloseReason::Full);
    }
    if !took_update(doc, seen, &payload) {
        return Ok(None);
    }
    if let Err(error) = store.append(Bytes::from(payload.clone())) {
        tracing::warn!(%error, "cannot append to the room file, the update is in memory only");
    } else if let Some(stamps) = stamps {
        let record = store.record_count().saturating_sub(1);
        stamps.append(author.stamp(record, now_ms(), &payload));
    }
    Ok(Some(Bytes::from(
        Message::Sync(SyncMessage::Update(payload)).encode_v1(),
    )))
}

/// Who an update comes from, as its stamp records it.
#[derive(Debug, Clone, Copy)]
enum Author {
    #[cfg(test)]
    Nobody,
    Client(Uuid),
    Restore {
        user: Uuid,
        version: u64,
    },
}

impl Author {
    fn stamp(self, record: u64, at_ms: i64, payload: &[u8]) -> Stamp {
        match self {
            #[cfg(test)]
            Self::Nobody => stamp_for(record, None, at_ms, payload),
            Self::Client(user) => stamp_for(record, Some(user), at_ms, payload),
            Self::Restore { user, version } => Stamp {
                restored_from: Some(version),
                ..stamp_for(record, Some(user), at_ms, payload)
            },
        }
    }
}

fn now_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

/// Applies one live update and says whether it changed the document: a block for a client
/// the room has not seen that far, or a deletion that removed something. `seen` is the
/// room's own state vector, raised only through the ranges that continue it, so an update
/// that arrives ahead of a gap is stored without claiming the blocks before the gap;
/// asking yrs (`state_vector`, `encode_update_v1`) instead would walk every client of the
/// document on every frame, which a client inventing ids can turn into quadratic work.
fn took_update(doc: &Doc, seen: &mut StateVector, payload: &[u8]) -> bool {
    // yrs asserts inside its decoders and its integrator on values it cannot represent, and
    // a record that cannot be applied must not take the room thread (or every later load)
    // with it.
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        take_record(doc, seen, payload)
    })) {
        Ok(changed) => changed,
        Err(_) => {
            tracing::warn!("skipping a record the document cannot represent");
            false
        }
    }
}

fn take_record(doc: &Doc, seen: &mut StateVector, payload: &[u8]) -> bool {
    #[cfg(test)]
    if payload == POISON_RECORD {
        panic!("a poisoned record");
    }
    let update = match Update::decode_v1(payload) {
        Ok(update) => update,
        Err(error) => {
            tracing::warn!(%error, "skipping a record that is not a v1 update");
            return false;
        }
    };
    let mut insertions: Vec<(ClientID, Vec<Range<u32>>)> = Vec::new();
    for (client, ranges) in update.insertions(true).iter() {
        insertions.push((*client, ranges.iter().cloned().collect()));
    }
    let carries_new = insertions
        .iter()
        .any(|(client, ranges)| ranges.iter().any(|range| range.end > seen.get(client)));
    let mut transaction = doc.transact_mut();
    if let Err(error) = transaction.apply_update(update) {
        tracing::warn!(%error, "skipping a record that does not apply");
        return false;
    }
    let changed = carries_new || !transaction.delete_set().is_empty();
    for (client, ranges) in &insertions {
        let seen_clock = seen.get(client);
        let mut clock = seen_clock;
        for range in ranges {
            if range.start > clock {
                break;
            }
            clock = clock.max(range.end);
        }
        if clock > seen_clock {
            seen.set_max(*client, clock);
        }
    }
    changed
}

/// Replays one stored record into a document that is being loaded; whether it changed
/// anything does not matter there.
fn replay(updates: &[Bytes]) -> Doc {
    let doc = Doc::new();
    for update in updates {
        let _ = apply(&doc, update);
    }
    doc
}

fn apply(doc: &Doc, payload: &[u8]) -> bool {
    took_update(doc, &mut StateVector::default(), payload)
}

#[cfg(test)]
pub(crate) const POISON_RECORD: &[u8] = b"a record only the tests write";

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

    async fn full_state(room: &RoomHandle, client: ClientId) -> Doc {
        let (out, mut inbox) = outbox();
        room.attach(
            client,
            Uuid::new_v4(),
            "test@example.org".to_owned(),
            Role::Editor,
            out,
        )
        .expect("the room is open");
        room.frame(
            client,
            Bytes::from(Message::Sync(SyncMessage::SyncStep1(StateVector::default())).encode_v1()),
        );
        let doc = Doc::new();
        while let Ok(Some(frame)) = timeout(WAIT, inbox.frames.recv()).await {
            if let Ok(Message::Sync(SyncMessage::SyncStep2(update))) = Message::decode_v1(&frame) {
                doc.transact_mut()
                    .apply_update(Update::decode_v1(&update).expect("a decodable update"))
                    .expect("an update that applies");
                break;
            }
        }
        let _ = room.detach(client).await;
        doc
    }

    fn has_key(doc: &Doc, key: &str) -> bool {
        doc.get_or_insert_map("content")
            .get(&doc.transact(), key)
            .is_some()
    }

    #[tokio::test]
    async fn a_panicking_command_closes_the_clients_and_unloads() {
        let dir = tempfile::tempdir().unwrap();
        let manager = RoomManager::new(dir.path().to_path_buf(), None);
        let room = manager.get_or_open("lab").unwrap();

        let owner = Uuid::new_v4();
        let (out, _owner) = outbox();
        room.attach(1, owner, "test@example.org".to_owned(), Role::Owner, out)
            .unwrap();
        let (out, _editor) = outbox();
        room.attach(2, owner, "test@example.org".to_owned(), Role::Editor, out)
            .unwrap();
        let (out, mut observer) = outbox();
        room.attach(3, owner, "test@example.org".to_owned(), Role::Viewer, out)
            .unwrap();

        room.frame(
            2,
            Bytes::from(Message::Sync(SyncMessage::Update(update_with("a", "1"))).encode_v1()),
        );
        let mut relayed = false;
        while let Ok(Some(frame)) = timeout(WAIT, observer.frames.recv()).await {
            if let Ok(Message::Sync(SyncMessage::Update(_))) = Message::decode_v1(&frame) {
                relayed = true;
                break;
            }
        }
        assert!(relayed, "the update reached the other client");

        room.panic();
        let closed = timeout(WAIT, observer.close)
            .await
            .expect("the room closes its clients")
            .expect("a reason");
        assert_eq!(closed.reason(), CloseReason::Failed.reason());
        // The thread closes its clients before it gives up its registry slot.
        timeout(WAIT, async {
            while manager.open_rooms() != 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the room unloads");

        let reopened = manager.get_or_open("lab").unwrap();
        let doc = full_state(&reopened, 3).await;
        assert!(has_key(&doc, "a"), "the pre-panic state came back");
    }

    #[test]
    fn a_poisoned_record_is_skipped_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _records) = FileRoomStore::open(dir.path(), "lab").unwrap();
        store.append(Bytes::from(update_with("a", "1"))).unwrap();
        store.append(Bytes::from_static(POISON_RECORD)).unwrap();
        store.write().unwrap();

        let (mut store, records) = FileRoomStore::open(dir.path(), "lab").unwrap();
        let doc = Doc::new();
        for record in records {
            let _ = apply(&doc, &record);
        }
        let mut clients = HashMap::new();
        let frame =
            Bytes::from(Message::Sync(SyncMessage::SyncStep1(StateVector::default())).encode_v1());
        let (out, mut inbox) = outbox();
        clients.insert(
            1,
            Client {
                user: Uuid::new_v4(),
                email: "test@example.org".to_owned(),
                role: Role::Owner,
                out,
            },
        );
        assert!(
            handle_frame(
                &mut Serving {
                    doc: &doc,
                    store: &mut store,
                    stamps: &mut None,
                    seen: &mut StateVector::default(),
                    full_state: &mut None,
                    clients: &mut clients,
                    presence: &mut Presence::default(),
                    limit: u64::MAX,
                },
                1,
                frame,
            )
            .unwrap()
            .is_none(),
            "a state request is answered to its asker only"
        );
        let answer = inbox.frames.try_recv().expect("the room serves its state");
        let Ok(Message::Sync(SyncMessage::SyncStep2(update))) = Message::decode_v1(&answer) else {
            panic!("the room answers a state request with an update");
        };
        doc.transact_mut()
            .apply_update(Update::decode_v1(&update).unwrap())
            .unwrap();
        assert!(has_key(&doc, "a"), "the good record survived the poison");

        let (out, _inbox) = outbox();
        clients.insert(
            2,
            Client {
                user: Uuid::new_v4(),
                email: "test@example.org".to_owned(),
                role: Role::Owner,
                out,
            },
        );
        let frame =
            Bytes::from(Message::Sync(SyncMessage::Update(update_with("b", "2"))).encode_v1());
        assert!(
            handle_frame(
                &mut Serving {
                    doc: &doc,
                    store: &mut store,
                    stamps: &mut None,
                    seen: &mut StateVector::default(),
                    full_state: &mut None,
                    clients: &mut clients,
                    presence: &mut Presence::default(),
                    limit: u64::MAX,
                },
                2,
                frame,
            )
            .unwrap()
            .is_some(),
            "the room still accepts updates"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cold_reads_are_bounded() {
        use std::sync::atomic::AtomicUsize;

        let dir = tempfile::tempdir().unwrap();
        let (mut store, _records) = FileRoomStore::open(dir.path(), "cold").unwrap();
        store
            .append(Bytes::from(update_with("key", "value")))
            .unwrap();
        store.write().unwrap();

        let manager = RoomManager::with_cold_reads(dir.path().to_path_buf(), None, 1);
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let reading = |enter: Arc<AtomicUsize>, top: Arc<AtomicUsize>| {
            let manager = manager.clone();
            async move {
                manager
                    .read("cold", move |doc| {
                        let now = enter.fetch_add(1, Ordering::SeqCst) + 1;
                        top.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(100));
                        enter.fetch_sub(1, Ordering::SeqCst);
                        has_key(doc, "key")
                    })
                    .await
            }
        };

        let (first, second) = tokio::join!(
            reading(Arc::clone(&in_flight), Arc::clone(&peak)),
            reading(Arc::clone(&in_flight), Arc::clone(&peak)),
        );

        assert!(first.unwrap(), "the first cold read saw the room");
        assert!(second.unwrap(), "the second cold read saw the room");
        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "two cold reads parsed the same room at once"
        );
    }

    #[tokio::test]
    async fn a_log_read_is_charged_for_each_document_it_builds() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _records) = FileRoomStore::open(dir.path(), "log").unwrap();
        store
            .append(Bytes::from(update_with("key", "value")))
            .unwrap();
        store.write().unwrap();
        let one_doc =
            fs::metadata(room_file(dir.path(), "log")).unwrap().len() * MEMORY_PER_FILE_BYTE;
        let manager = RoomManager::new(dir.path().to_path_buf(), Some(one_doc * 3 / 2));

        let one = manager.read_log("log", 1, |records, _| records.len()).await;
        assert_eq!(one.unwrap(), 1);
        let two = manager.read_log("log", 2, |records, _| records.len()).await;
        assert!(
            matches!(two, Err(RoomError::OverBudget { .. })),
            "two documents fit a budget for one and a half: {two:?}"
        );
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
                email: "test@example.org".to_owned(),
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
                &mut Serving {
                    doc: &doc,
                    store: &mut store,
                    stamps: &mut None,
                    seen: &mut StateVector::default(),
                    full_state: &mut None,
                    clients: &mut clients,
                    presence: &mut Presence::default(),
                    limit: u64::MAX,
                },
                7,
                Bytes::from(hostile),
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn a_queued_attach_keeps_a_dying_room_alive() {
        let mut room = dying_room();
        let (out, _inbox) = outbox();
        room.handle
            .attach(
                1,
                Uuid::new_v4(),
                "test@example.org".to_owned(),
                Role::Editor,
                out,
            )
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
            room.handle.attach(
                1,
                Uuid::new_v4(),
                "test@example.org".to_owned(),
                Role::Editor,
                out
            ),
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
        room.attach(
            1,
            Uuid::new_v4(),
            "test@example.org".to_owned(),
            Role::Editor,
            out,
        )
        .unwrap();
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
        room.attach(
            client,
            Uuid::new_v4(),
            "test@example.org".to_owned(),
            Role::Editor,
            out,
        )
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

    #[test]
    fn repeated_update_appends_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, records) = FileRoomStore::open(dir.path(), "room-repeat").unwrap();
        assert!(records.is_empty());
        let doc = Doc::new();
        let mut seen = StateVector::default();
        let update = update_with("key", "value");

        assert!(
            handle_update(
                &doc,
                &mut store,
                &mut None,
                &mut seen,
                u64::MAX,
                update.clone(),
                Author::Nobody
            )
            .unwrap()
            .is_some(),
            "the first update is broadcast"
        );
        assert!(
            handle_update(
                &doc,
                &mut store,
                &mut None,
                &mut seen,
                u64::MAX,
                update,
                Author::Nobody
            )
            .unwrap()
            .is_none(),
            "a repeated update changes nothing and is broadcast again"
        );
        store.write().unwrap();

        let (_, records) = FileRoomStore::open(dir.path(), "room-repeat").unwrap();
        assert_eq!(
            records.len(),
            1,
            "the repeated update was written to the file"
        );
    }

    #[test]
    fn out_of_order_updates_survive_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _records) = FileRoomStore::open(dir.path(), "room-out-of-order").unwrap();
        let doc = Doc::new();
        let mut seen = StateVector::default();

        let writer = Doc::new();
        let content = writer.get_or_insert_map("content");
        content.insert(&mut writer.transact_mut(), "first", "value");
        let first = writer
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        let after_first = writer.transact().state_vector();
        content.insert(&mut writer.transact_mut(), "second", "value");
        let second = writer.transact().encode_state_as_update_v1(&after_first);

        assert!(
            handle_update(
                &doc,
                &mut store,
                &mut None,
                &mut seen,
                u64::MAX,
                second,
                Author::Nobody
            )
            .unwrap()
            .is_some(),
            "an update ahead of its neighbour is kept"
        );
        assert!(
            handle_update(
                &doc,
                &mut store,
                &mut None,
                &mut seen,
                u64::MAX,
                first,
                Author::Nobody
            )
            .unwrap()
            .is_some(),
            "the update that fills the gap is kept"
        );
        store.write().unwrap();

        let (_, records) = FileRoomStore::open(dir.path(), "room-out-of-order").unwrap();
        let reopened = Doc::new();
        for record in &records {
            apply(&reopened, record);
        }
        assert_eq!(
            reopened.transact().state_vector(),
            writer.transact().state_vector(),
            "the reopened room holds every block the writer made"
        );
        assert!(
            has_key(&reopened, "first") && has_key(&reopened, "second"),
            "both edits survive the reopen"
        );
    }

    #[test]
    fn delete_only_update_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _records) = FileRoomStore::open(dir.path(), "room-delete").unwrap();
        let doc = Doc::new();
        let mut seen = StateVector::default();
        let insert = update_with("key", "value");
        handle_update(
            &doc,
            &mut store,
            &mut None,
            &mut seen,
            u64::MAX,
            insert.clone(),
            Author::Nobody,
        )
        .unwrap()
        .expect("the insert is broadcast");

        let writer = Doc::new();
        writer
            .transact_mut()
            .apply_update(Update::decode_v1(&insert).unwrap())
            .unwrap();
        let content = writer.get_or_insert_map("content");
        let mut txn = writer.transact_mut();
        content.remove(&mut txn, "key");
        let deletion = txn.encode_update_v1();
        drop(txn);

        assert!(
            handle_update(
                &doc,
                &mut store,
                &mut None,
                &mut seen,
                u64::MAX,
                deletion,
                Author::Nobody
            )
            .unwrap()
            .is_some(),
            "an update that only deletes is broadcast"
        );
        store.write().unwrap();

        let (_, records) = FileRoomStore::open(dir.path(), "room-delete").unwrap();
        assert_eq!(records.len(), 2, "the deletion was written to the file");
        let replayed = Doc::new();
        for record in records {
            replayed
                .transact_mut()
                .apply_update(Update::decode_v1(&record).unwrap())
                .unwrap();
        }
        assert!(
            !has_key(&replayed, "key"),
            "the deletion did not survive the reopen"
        );
    }

    #[test]
    fn repeated_delete_only_update_appends_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _records) = FileRoomStore::open(dir.path(), "room-redelete").unwrap();
        let doc = Doc::new();
        let mut seen = StateVector::default();
        let insert = update_with("key", "value");
        handle_update(
            &doc,
            &mut store,
            &mut None,
            &mut seen,
            u64::MAX,
            insert.clone(),
            Author::Nobody,
        )
        .unwrap()
        .expect("the insert is broadcast");

        let writer = Doc::new();
        writer
            .transact_mut()
            .apply_update(Update::decode_v1(&insert).unwrap())
            .unwrap();
        let content = writer.get_or_insert_map("content");
        let mut txn = writer.transact_mut();
        content.remove(&mut txn, "key");
        let deletion = txn.encode_update_v1();
        drop(txn);

        assert!(
            handle_update(
                &doc,
                &mut store,
                &mut None,
                &mut seen,
                u64::MAX,
                deletion.clone(),
                Author::Nobody
            )
            .unwrap()
            .is_some(),
            "an update that only deletes is broadcast"
        );
        assert!(
            handle_update(
                &doc,
                &mut store,
                &mut None,
                &mut seen,
                u64::MAX,
                deletion,
                Author::Nobody
            )
            .unwrap()
            .is_none(),
            "a repeated delete changes nothing and is broadcast again"
        );
        store.write().unwrap();

        let (_, records) = FileRoomStore::open(dir.path(), "room-redelete").unwrap();
        assert_eq!(
            records.len(),
            2,
            "the repeated delete was written to the file"
        );
    }

    #[test]
    fn partly_new_update_is_stored() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _records) = FileRoomStore::open(dir.path(), "room-partly").unwrap();
        let doc = Doc::new();
        let mut seen = StateVector::default();
        let first = update_with("old", "value");
        handle_update(
            &doc,
            &mut store,
            &mut None,
            &mut seen,
            u64::MAX,
            first.clone(),
            Author::Nobody,
        )
        .unwrap()
        .expect("the first update is broadcast");

        let producer = Doc::new();
        producer
            .transact_mut()
            .apply_update(Update::decode_v1(&first).unwrap())
            .unwrap();
        producer
            .get_or_insert_map("content")
            .insert(&mut producer.transact_mut(), "new", "value");
        let combined = producer
            .transact()
            .encode_state_as_update_v1(&StateVector::default());

        assert!(
            handle_update(
                &doc,
                &mut store,
                &mut None,
                &mut seen,
                u64::MAX,
                combined,
                Author::Nobody
            )
            .unwrap()
            .is_some(),
            "an update with new blocks is broadcast even when it repeats old ones"
        );
        store.write().unwrap();

        let (_, records) = FileRoomStore::open(dir.path(), "room-partly").unwrap();
        assert_eq!(records.len(), 2, "the partly new update was not written");
        let replayed = Doc::new();
        for record in records {
            replayed
                .transact_mut()
                .apply_update(Update::decode_v1(&record).unwrap())
                .unwrap();
        }
        assert!(has_key(&replayed, "old"), "the old key did not survive");
        assert!(has_key(&replayed, "new"), "the new key did not survive");
    }

    #[test]
    fn incremental_update_is_stored() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _records) = FileRoomStore::open(dir.path(), "room-incremental").unwrap();
        let doc = Doc::new();
        let mut seen = StateVector::default();
        let producer = Doc::new();
        let content = producer.get_or_insert_map("content");

        let first = {
            let mut transaction = producer.transact_mut();
            content.insert(&mut transaction, "one", "1");
            transaction.encode_update_v1()
        };
        assert!(
            handle_update(
                &doc,
                &mut store,
                &mut None,
                &mut seen,
                u64::MAX,
                first,
                Author::Nobody
            )
            .unwrap()
            .is_some(),
            "the first update is broadcast"
        );
        let second = {
            let mut transaction = producer.transact_mut();
            content.insert(&mut transaction, "two", "2");
            transaction.encode_update_v1()
        };
        assert!(
            handle_update(
                &doc,
                &mut store,
                &mut None,
                &mut seen,
                u64::MAX,
                second,
                Author::Nobody
            )
            .unwrap()
            .is_some(),
            "an update that continues a client's history is broadcast"
        );
        store.write().unwrap();

        let (_, records) = FileRoomStore::open(dir.path(), "room-incremental").unwrap();
        assert_eq!(records.len(), 2, "the incremental update was not written");
        let replayed = Doc::new();
        for record in records {
            replayed
                .transact_mut()
                .apply_update(Update::decode_v1(&record).unwrap())
                .unwrap();
        }
        assert!(has_key(&replayed, "one"), "the first key did not survive");
        assert!(has_key(&replayed, "two"), "the second key did not survive");
    }

    #[test]
    fn update_past_the_room_limit_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _records) = FileRoomStore::open(dir.path(), "room-limit").unwrap();
        let doc = Doc::new();
        let mut seen = StateVector::default();
        let mut full_state = None;
        let mut clients = HashMap::new();
        for client in 1..=2 {
            let (out, _inbox) = outbox();
            clients.insert(
                client,
                Client {
                    user: Uuid::new_v4(),
                    email: "test@example.org".to_owned(),
                    role: Role::Owner,
                    out,
                },
            );
        }
        let frame = |payload: &[u8]| {
            Bytes::from(Message::Sync(SyncMessage::Update(payload.to_vec())).encode_v1())
        };
        let small = update_with("key", "value");
        let limit = store.total_size() + 2 * small.len() as u64 + 8;
        let accepted = handle_frame(
            &mut Serving {
                doc: &doc,
                store: &mut store,
                stamps: &mut None,
                seen: &mut seen,
                full_state: &mut full_state,
                clients: &mut clients,
                presence: &mut Presence::default(),
                limit,
            },
            1,
            frame(&small),
        )
        .expect("a small update fits");
        assert!(accepted.is_some(), "the small update is broadcast");

        let before = store.total_size();
        let big = update_with("big", &"x".repeat(4096));
        assert_eq!(
            handle_frame(
                &mut Serving {
                    doc: &doc,
                    store: &mut store,
                    stamps: &mut None,
                    seen: &mut seen,
                    full_state: &mut full_state,
                    clients: &mut clients,
                    presence: &mut Presence::default(),
                    limit,
                },
                2,
                frame(&big),
            ),
            Err(CloseReason::Full),
            "an update past the limit is refused"
        );
        assert_eq!(
            store.total_size(),
            before,
            "a refused update is not appended"
        );
        assert!(!has_key(&doc, "big"), "a refused update is not applied");

        let later = update_with("second", "value");
        let accepted = handle_frame(
            &mut Serving {
                doc: &doc,
                store: &mut store,
                stamps: &mut None,
                seen: &mut seen,
                full_state: &mut full_state,
                clients: &mut clients,
                presence: &mut Presence::default(),
                limit,
            },
            1,
            frame(&later),
        )
        .expect("another client still writes under the limit");
        assert!(accepted.is_some(), "the later small update is broadcast");
    }

    #[test]
    fn the_stamps_count_against_the_room_limit() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _records) = FileRoomStore::open(dir.path(), "lab").unwrap();
        let mut stamps = Some(StampFile::open(dir.path(), "lab").unwrap());
        let (doc, mut seen) = (Doc::new(), StateVector::default());
        let first = update_with("key", "value");
        handle_update(
            &doc,
            &mut store,
            &mut stamps,
            &mut seen,
            u64::MAX,
            first,
            Author::Nobody,
        )
        .unwrap();

        let second = update_with("second", "value");
        let limit = store.total_size() + second.len() as u64 + 8;
        assert_eq!(
            handle_update(
                &doc,
                &mut store,
                &mut stamps,
                &mut seen,
                limit,
                second,
                Author::Nobody
            ),
            Err(CloseReason::Full),
            "the room file fits, its stamps do not"
        );
    }

    #[tokio::test]
    async fn owner_quota_counts_other_rooms_stamps() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::open(&dir.path().join("test.db")).await.unwrap();
        let owner = crate::db::create_user(&pool, "owner@example.org", "pwd", false, true)
            .await
            .unwrap();
        for room in ["first", "second"] {
            crate::db::create_room(&pool, owner, room, room)
                .await
                .unwrap();
        }
        fs::write(stamps_file(dir.path(), "first"), [0u8; 96]).unwrap();
        let manager = RoomManager::new(dir.path().to_owned(), None).with_storage(StorageLimits {
            room: u64::MAX,
            owner: 196,
        });

        assert_eq!(manager.allowance(&pool, "second").await.unwrap(), 100);
        assert_eq!(manager.room_size("first"), 96);
    }

    #[tokio::test]
    async fn owner_quota_counts_other_rooms() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::open(&dir.path().join("test.db")).await.unwrap();
        let owner = crate::db::create_user(&pool, "owner@example.org", "pwd", false, true)
            .await
            .unwrap();
        crate::db::create_room(&pool, owner, "first", "First")
            .await
            .unwrap();
        crate::db::create_room(&pool, owner, "second", "Second")
            .await
            .unwrap();
        let (mut store, _records) = FileRoomStore::open(dir.path(), "first").unwrap();
        store
            .append(Bytes::from(update_with("key", "value")))
            .unwrap();
        store.write().unwrap();
        let used = fs::metadata(room_file(dir.path(), "first")).unwrap().len();
        let manager = RoomManager::new(dir.path().to_owned(), None).with_storage(StorageLimits {
            room: u64::MAX,
            owner: used + 100,
        });

        assert_eq!(
            manager.allowance(&pool, "second").await.unwrap(),
            100,
            "the owner's other room did not count against the quota"
        );
    }
}
