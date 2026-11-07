//! A room: one yrs document, its local cache and the background task that keeps it in step
//! with the server.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use cocat_core::model;
use cocat_core::store::{FileRoomStore, StoreError};
use thiserror::Error;
use tokio::sync::{Notify, broadcast};
use url::Url;
use yrs::types::DeepObservable;
use yrs::updates::decoder::Decode;
use yrs::{Doc, Origin, ReadTxn, StateVector, Transact, TransactionMut, Update, WriteTxn};

use crate::changes::{self, Change, Observation, Root};
use crate::login::Credentials;

/// The origin of a transaction that applies what the server sent.
pub(crate) const REMOTE_ORIGIN: &str = "cocat-client:remote";
/// The origin of a transaction a caller opened through [`Room::write`].
pub(crate) const LOCAL_ORIGIN: &str = "cocat-client:local";
/// The origin of the transaction that replays the cache when a room opens.
pub(crate) const REPLAY_ORIGIN: &str = "cocat-client:replay";

/// How many change batches a slow listener may fall behind before it starts missing them.
const CHANGES: usize = 1024;

/// Where the room stands with the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// No session is running: the room works from its cache.
    Offline,
    /// A session is being opened, or was lost and is being retried.
    Connecting,
    /// The room has exchanged its state with the server at least once this session.
    Synced,
    /// The server refused the session; the room will not retry until it is given a new cookie.
    Unauthorized,
}

#[derive(Debug, Error)]
pub enum RoomError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("the cache holds a record that is not a document update")]
    Cache,
    #[error("the cache of the first layout could not be removed: {0}")]
    Discard(#[source] std::io::Error),
    #[error(transparent)]
    Url(#[from] url::ParseError),
    #[error("a room url must be http, https, ws or wss")]
    Scheme,
    #[error("the sync thread could not start: {0}")]
    Runtime(#[source] std::io::Error),
    #[error("the document refused an observer")]
    Observer,
    #[error("the server refused the session")]
    Unauthorized,
    #[error("the room did not reach {0:?} in time")]
    TimedOut(Status),
    #[error("the room is not connected")]
    Offline,
    #[error("the server did not acknowledge the room's changes in time")]
    Unacknowledged,
}

/// A room: the document, the cache and the session to the server.
pub struct Room {
    inner: Arc<Inner>,
    session: Mutex<Option<Session>>,
}

/// What the sync task and the document observers share.
pub(crate) struct Inner {
    pub(crate) doc: Doc,
    pub(crate) room_id: String,
    pub(crate) cache: Arc<Mutex<FileRoomStore>>,
    pub(crate) outbox: Arc<Outbox>,
    /// Whether a session socket is open; only then can a local update be sent at once.
    pub(crate) online: Arc<AtomicBool>,
    pub(crate) changes: broadcast::Sender<Vec<Change>>,
    pub(crate) pending: Arc<Mutex<Vec<Observation>>>,
    pub(crate) subscribers: Subscribers,
    /// While a batch is open, local updates are collected here instead of queued at once.
    pub(crate) batching: Arc<AtomicBool>,
    pub(crate) batched: Arc<Mutex<Vec<Vec<u8>>>>,
    pub(crate) status: StatusCell,
    pub(crate) ack: AckCell,
}

/// Every listener of a room's changes, called on the thread that applied them.
pub(crate) type Subscribers = Arc<Mutex<Vec<Arc<dyn Fn(Vec<Change>) + Send + Sync>>>>;

/// Something the session has to send, in the order the document produced it.
pub(crate) enum Outgoing {
    /// A local transaction's update.
    Update(Vec<u8>),
    /// Asks the session for a state vector once everything before it is sent: the server's
    /// answer then proves it applied those updates.
    Ack,
}

/// The local updates waiting to be sent, from whatever thread made them.
pub(crate) struct Outbox {
    updates: Mutex<Vec<Outgoing>>,
    ready: Notify,
}

impl Outbox {
    fn new() -> Outbox {
        Outbox {
            updates: Mutex::new(Vec::new()),
            ready: Notify::new(),
        }
    }

    pub(crate) fn push(&self, update: Vec<u8>) {
        lock(&self.updates).push(Outgoing::Update(update));
        self.ready.notify_one();
    }

    pub(crate) fn request_ack(&self) {
        lock(&self.updates).push(Outgoing::Ack);
        self.ready.notify_one();
    }

    /// Resolves once an update is waiting, or at once if one already is.
    pub(crate) async fn ready(&self) {
        self.ready.notified().await;
    }

    pub(crate) fn drain(&self) -> Vec<Outgoing> {
        std::mem::take(&mut *lock(&self.updates))
    }
}

/// How many times the server has answered a state vector, and a way to wait for the next one.
pub(crate) struct AckCell {
    generation: Mutex<u64>,
    changed: Condvar,
}

impl AckCell {
    fn new() -> AckCell {
        AckCell {
            generation: Mutex::new(0),
            changed: Condvar::new(),
        }
    }

    pub(crate) fn generation(&self) -> u64 {
        *lock(&self.generation)
    }

    pub(crate) fn acknowledge(&self) {
        *lock(&self.generation) += 1;
        self.changed.notify_all();
    }

    fn wait(&self, generation: u64, timeout: Duration) -> Result<(), RoomError> {
        let deadline = Instant::now() + timeout;
        let mut seen = lock(&self.generation);
        while *seen <= generation {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(RoomError::Unacknowledged);
            }
            let (next, _) = self
                .changed
                .wait_timeout(seen, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            seen = next;
        }
        Ok(())
    }
}

/// The status, and a way for a blocking caller to wait for one.
pub(crate) struct StatusCell {
    status: Mutex<Status>,
    changed: Condvar,
}

impl StatusCell {
    fn new() -> StatusCell {
        StatusCell {
            status: Mutex::new(Status::Offline),
            changed: Condvar::new(),
        }
    }

    pub(crate) fn get(&self) -> Status {
        *lock(&self.status)
    }

    pub(crate) fn set(&self, status: Status) {
        *lock(&self.status) = status;
        self.changed.notify_all();
    }

    fn wait(&self, wanted: Status, timeout: Duration) -> Result<(), RoomError> {
        let deadline = Instant::now() + timeout;
        let mut status = lock(&self.status);
        while *status != wanted {
            if *status == Status::Unauthorized {
                return Err(RoomError::Unauthorized);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(RoomError::TimedOut(wanted));
            }
            let (next, _) = self
                .changed
                .wait_timeout(status, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            status = next;
        }
        Ok(())
    }
}

/// The running sync task, and the runtime it was spawned on; the room owns the runtime so a
/// caller needs no async setup to use a room.
struct Session {
    runtime: tokio::runtime::Runtime,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Room {
    fn drop(&mut self) {
        self.close();
    }
}

impl Room {
    /// Opens the cache of one room. Everything the cache holds is in the document by the time
    /// this returns, and the room works offline until [`Room::connect`].
    pub fn open(cache_dir: &Path, room_id: &str) -> Result<Room, RoomError> {
        let (store, doc) = load_cache(cache_dir, room_id)?;
        let (changes, _) = broadcast::channel(CHANGES);
        let inner = Arc::new(Inner {
            doc,
            room_id: room_id.to_owned(),
            cache: Arc::new(Mutex::new(store)),
            outbox: Arc::new(Outbox::new()),
            online: Arc::new(AtomicBool::new(false)),
            changes,
            pending: Arc::new(Mutex::new(Vec::new())),
            subscribers: Arc::new(Mutex::new(Vec::new())),
            batching: Arc::new(AtomicBool::new(false)),
            batched: Arc::new(Mutex::new(Vec::new())),
            status: StatusCell::new(),
            ack: AckCell::new(),
        });
        observe(&inner)?;
        Ok(Room {
            inner,
            session: Mutex::new(None),
        })
    }

    /// Runs `edit` in one transaction, which is one update for the server and for the cache.
    pub fn write<T>(&self, edit: impl FnOnce(&mut TransactionMut<'_>) -> T) -> T {
        let mut txn = self.inner.doc.transact_mut_with(LOCAL_ORIGIN);
        edit(&mut txn)
    }

    /// Calls `listener` with every batch of remote changes, on the thread that applied them.
    pub fn subscribe(&self, listener: impl Fn(Vec<Change>) + Send + Sync + 'static) {
        lock(&self.inner.subscribers).push(Arc::new(listener));
    }

    /// Starts one batch: the local edits that follow are sent to the server as one update,
    /// so the others see them in one transaction.
    pub fn begin_batch(&self) {
        lock(&self.inner.batched).clear();
        self.inner.batching.store(true, Ordering::Relaxed);
    }

    /// Ends the batch opened by [`Room::begin_batch`] and queues its merged update.
    pub fn end_batch(&self) {
        self.inner.batching.store(false, Ordering::Relaxed);
        let updates = std::mem::take(&mut *lock(&self.inner.batched));
        if updates.is_empty() {
            return;
        }
        let merged = match merge(&updates) {
            Some(merged) => merged,
            None => {
                tracing::error!("the batch could not be merged; sending its parts");
                for update in updates {
                    self.queue(update);
                }
                return;
            }
        };
        self.queue(merged);
    }

    fn queue(&self, update: Vec<u8>) {
        if self.inner.online.load(Ordering::Relaxed) {
            self.inner.outbox.push(update);
        }
    }

    pub fn doc(&self) -> &Doc {
        &self.inner.doc
    }

    pub fn room_id(&self) -> &str {
        &self.inner.room_id
    }

    pub fn status(&self) -> Status {
        self.inner.status.get()
    }

    pub fn changes(&self) -> broadcast::Receiver<Vec<Change>> {
        self.inner.changes.subscribe()
    }

    /// Blocks until the room reaches `status`, or the timeout passes.
    pub fn wait_for(&self, status: Status, timeout: Duration) -> Result<(), RoomError> {
        self.inner.status.wait(status, timeout)
    }

    /// Blocks until the room has exchanged its state with the server at least once.
    pub fn wait_synced(&self, timeout: Duration) -> Result<(), RoomError> {
        self.wait_for(Status::Synced, timeout)
    }

    /// Blocks until the server has processed every local change made so far.
    ///
    /// The session sends a state vector after those changes, and the server handles one
    /// socket's frames in order, so its answer to that vector is the proof.
    pub fn save(&self, timeout: Duration) -> Result<(), RoomError> {
        if !self.inner.online.load(Ordering::Relaxed) {
            return Err(RoomError::Offline);
        }
        let generation = self.inner.ack.generation();
        self.inner.outbox.request_ack();
        self.inner.ack.wait(generation, timeout)
    }

    /// Starts a session on a runtime the room owns and returns at once. A previous session is
    /// dropped first, so this doubles as "use this cookie now".
    pub fn connect(&self, url: &str, cookie: &str) -> Result<(), RoomError> {
        self.connect_with(url, cookie, None)
    }

    /// Like [`Room::connect`], and when the server refuses the cookie the session logs in
    /// again with `credentials`, once, before it gives up as [`Status::Unauthorized`].
    pub fn connect_with(
        &self,
        url: &str,
        cookie: &str,
        credentials: Option<Credentials>,
    ) -> Result<(), RoomError> {
        let url = websocket_url(url, &self.inner.room_id)?;
        self.stop();
        // Set before spawning: a caller that just gave a new cookie must not read the old
        // refusal while the new session is still starting.
        self.inner.status.set(Status::Connecting);

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(RoomError::Runtime)?;
        let inner = Arc::clone(&self.inner);
        let cookie = cookie.to_owned();
        let task = runtime.spawn(crate::sync::run(inner, url, cookie, credentials));
        *lock(&self.session) = Some(Session { runtime, task });
        Ok(())
    }

    /// Stops the session and writes the cache to the disk.
    pub fn close(&self) {
        self.stop();
        self.inner.status.set(Status::Offline);
        if let Err(error) = lock(&self.inner.cache).flush() {
            tracing::error!(%error, "the cache could not be flushed");
        }
    }

    fn stop(&self) {
        let Some(session) = lock(&self.session).take() else {
            return;
        };
        self.inner.online.store(false, Ordering::Relaxed);
        session.task.abort();
        // Dropping a runtime blocks until its threads stop, which panics when the room is
        // closed from inside another runtime; the cache is already on disk at this point.
        session.runtime.shutdown_background();
    }
}

/// The cache file and the document it holds. A cache of the first layout is discarded: it is
/// only a copy of the server's room, which a v2 server serves migrated.
fn load_cache(cache_dir: &Path, room_id: &str) -> Result<(FileRoomStore, Doc), RoomError> {
    let (store, records) = FileRoomStore::open(cache_dir, room_id)?;
    let doc = replay(records)?;
    if !model::is_v1(&doc.transact()) {
        return Ok((store, doc));
    }
    tracing::info!(room = room_id, "discarding a cache of the first layout");
    let path = store.path().to_owned();
    drop(store);
    fs::remove_file(&path).map_err(RoomError::Discard)?;
    let (store, _) = FileRoomStore::open(cache_dir, room_id)?;
    Ok((store, Doc::new()))
}

fn replay(records: Vec<Bytes>) -> Result<Doc, RoomError> {
    let doc = Doc::new();
    for record in records {
        let update = Update::decode_v1(&record).map_err(|_| RoomError::Cache)?;
        doc.transact_mut_with(REPLAY_ORIGIN)
            .apply_update(update)
            .map_err(|_| RoomError::Cache)?;
    }
    Ok(doc)
}

/// One update holding what every update holds, so the others see one transaction.
fn merge(updates: &[Vec<u8>]) -> Option<Vec<u8>> {
    let doc = Doc::new();
    let mut txn = doc.transact_mut();
    for update in updates {
        let update = Update::decode_v1(update).ok()?;
        txn.apply_update(update).ok()?;
    }
    drop(txn);
    Some(
        doc.transact()
            .encode_state_as_update_v1(&StateVector::default()),
    )
}

/// Registers the observers that cache, forward and translate document changes.
fn observe(inner: &Arc<Inner>) -> Result<(), RoomError> {
    // The deep observers come first: getting a root map out of the document is itself a
    // transaction, and nothing a room opens should look like a local edit to the cache.
    for (root, name) in [
        (Root::Catalogues, model::CATALOGUES),
        (Root::Events, model::EVENTS),
    ] {
        let pending = Arc::clone(&inner.pending);
        let map = inner
            .doc
            .transact_mut_with(REPLAY_ORIGIN)
            .get_or_insert_map(name);
        map.observe_deep("cocat-client:changes", move |txn, events| {
            if !is_remote(txn) {
                return;
            }
            changes::observe(root, events, txn, &mut lock(&pending));
        });
    }

    let cache = Arc::clone(&inner.cache);
    let outbox = Arc::clone(&inner.outbox);
    let online = Arc::clone(&inner.online);
    let batching = Arc::clone(&inner.batching);
    let batched = Arc::clone(&inner.batched);
    inner
        .doc
        .observe_update_v1("cocat-client:cache", move |_txn, update| {
            // Every update is appended, local or remote: the cache is the room's copy on disk,
            // so a room opens offline with what the server already sent and a reconnect asks
            // the server only for what changed since.
            // simplify: the file only ever grows. Rewriting it as one state update when a room
            // opens and the file is much larger than that (and compaction is wanted back) is
            // the upgrade path.
            if let Err(error) = lock(&cache).append(Bytes::copy_from_slice(&update.update)) {
                tracing::error!(%error, "the cache rejected an update");
                return;
            }
            if let Err(error) = lock(&cache).write() {
                tracing::error!(%error, "the cache could not write an update");
            }
            if batching.load(Ordering::Relaxed) {
                lock(&batched).push(update.update.clone());
                return;
            }
            // Offline work needs no outbox: the handshake diff carries it to the server on
            // the next connect, so queuing it would only grow without a reader.
            if online.load(Ordering::Relaxed) {
                outbox.push(update.update.clone());
            }
        })
        .map_err(|_| RoomError::Observer)?;

    let pending = Arc::clone(&inner.pending);
    let sender = inner.changes.clone();
    let subscribers = Arc::clone(&inner.subscribers);
    inner
        .doc
        .observe_after_transaction("cocat-client:batch", move |txn| {
            let observations = std::mem::take(&mut *lock(&pending));
            if observations.is_empty() {
                return;
            }
            let batch = changes::finish(&observations, txn);
            if batch.is_empty() {
                return;
            }
            for subscriber in lock(&subscribers).iter() {
                subscriber(batch.clone());
            }
            // A closed channel means nobody listens any more; the changes are not news.
            let _ = sender.send(batch);
        })
        .map_err(|_| RoomError::Observer)?;
    Ok(())
}

/// Whether this transaction applies what the server sent.
pub(crate) fn is_remote(txn: &TransactionMut<'_>) -> bool {
    txn.origin()
        .is_some_and(|origin| *origin == Origin::from(REMOTE_ORIGIN))
}

/// The room's websocket address, keeping any path prefix the server url carries.
fn websocket_url(base: &str, room_id: &str) -> Result<Url, RoomError> {
    let mut url = Url::parse(base)?;
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        "ws" => "ws",
        "wss" => "wss",
        _ => return Err(RoomError::Scheme),
    };
    url.set_scheme(scheme).map_err(|_| RoomError::Scheme)?;
    url.path_segments_mut()
        .map_err(|_| RoomError::Scheme)?
        .pop_if_empty()
        .push("room")
        .push(room_id);
    Ok(url)
}

/// Locks, treating a poisoned mutex as usable: the data is a plain value or a record list, and
/// a panic elsewhere must not turn every later change into a panic of its own.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
