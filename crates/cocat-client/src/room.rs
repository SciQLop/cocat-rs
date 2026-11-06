//! A room: one yrs document, its local cache and the background task that keeps it in step
//! with the server.

use std::path::Path;
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
use yrs::{Doc, Origin, Transact, TransactionMut, Update, WriteTxn};

use crate::changes::{self, Change, Observation, Root};

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
    pub(crate) changes: broadcast::Sender<Vec<Change>>,
    pub(crate) pending: Arc<Mutex<Vec<Observation>>>,
    pub(crate) status: StatusCell,
}

/// The local updates waiting to be sent, from whatever thread made them.
pub(crate) struct Outbox {
    updates: Mutex<Vec<Vec<u8>>>,
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
        lock(&self.updates).push(update);
        self.ready.notify_one();
    }

    /// Resolves once an update is waiting, or at once if one already is.
    pub(crate) async fn ready(&self) {
        self.ready.notified().await;
    }

    pub(crate) fn drain(&self) -> Vec<Vec<u8>> {
        std::mem::take(&mut *lock(&self.updates))
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
        let (store, records) = FileRoomStore::open(cache_dir, room_id)?;
        let doc = Doc::new();
        for record in records {
            let update = Update::decode_v1(&record).map_err(|_| RoomError::Cache)?;
            doc.transact_mut_with(REPLAY_ORIGIN)
                .apply_update(update)
                .map_err(|_| RoomError::Cache)?;
        }

        let (changes, _) = broadcast::channel(CHANGES);
        let inner = Arc::new(Inner {
            doc,
            room_id: room_id.to_owned(),
            cache: Arc::new(Mutex::new(store)),
            outbox: Arc::new(Outbox::new()),
            changes,
            pending: Arc::new(Mutex::new(Vec::new())),
            status: StatusCell::new(),
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

    /// Starts a session on a runtime the room owns and returns at once. A previous session is
    /// dropped first, so this doubles as "use this cookie now".
    pub fn connect(&self, url: &str, cookie: &str) -> Result<(), RoomError> {
        let url = websocket_url(url, &self.inner.room_id)?;
        self.stop();

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(RoomError::Runtime)?;
        let inner = Arc::clone(&self.inner);
        let cookie = cookie.to_owned();
        let task = runtime.spawn(crate::sync::run(inner, url, cookie));
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
        session.task.abort();
        // Dropping a runtime blocks until its threads stop, which panics when the room is
        // closed from inside another runtime; the cache is already on disk at this point.
        session.runtime.shutdown_background();
    }
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
    inner
        .doc
        .observe_update_v1("cocat-client:cache", move |txn, update| {
            if is_remote(txn) {
                // The server already has it, and the cache only holds what the room still
                // has to send.
                return;
            }
            if let Err(error) = lock(&cache).append(Bytes::copy_from_slice(&update.update)) {
                tracing::error!(%error, "the cache rejected an update");
                return;
            }
            if let Err(error) = lock(&cache).write() {
                tracing::error!(%error, "the cache could not write an update");
            }
            outbox.push(update.update.clone());
        })
        .map_err(|_| RoomError::Observer)?;

    let pending = Arc::clone(&inner.pending);
    let sender = inner.changes.clone();
    inner
        .doc
        .observe_after_transaction("cocat-client:batch", move |txn| {
            let observations = std::mem::take(&mut *lock(&pending));
            if observations.is_empty() {
                return;
            }
            let batch = changes::finish(&observations, txn);
            if !batch.is_empty() {
                // A closed channel means nobody listens any more; the changes are not news.
                let _ = sender.send(batch);
            }
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
