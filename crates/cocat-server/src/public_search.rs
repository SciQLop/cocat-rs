//! `GET /api/public/events`: one search through the events of every public catalogue, the
//! scope a smart catalogue takes to watch them all.
//!
//! The public catalogues hold over a million events; read into memory they take gigabytes.
//! Instead each one is written once to a snapshot file, one JSON event per line, rewritten
//! when its room file changes, and a search scans the snapshots in parallel. What a search
//! found is kept per catalogue: when one catalogue changes, a search seen before scans only
//! that one again. A version tag over the catalogues' stamps lets a client that holds the
//! answer skip it.
//!
//! Snapshots are written in the background, one at a time: loading the largest room takes
//! minutes and gigabytes. A search never waits long for them: it answers from the snapshots
//! there are, an out-of-date one included, and says how many catalogues are not indexed yet.

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use cocat_core::model::{self, EventRecord};
use cocat_core::rules::{Lookups, Predicate, relative};
use cocat_core::time::Timestamp;
use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use yrs::Transact;

use crate::AppState;
use crate::api::{ApiError, CurrentUser};
use crate::db;
use crate::openapi::Detail;
use crate::public_api::file_stamp;

const DEFAULT_LIMIT: usize = 1000;
const MAX_LIMIT: usize = 10_000;
/// How long a search waits for the snapshots it lacks before it answers without them.
const WAIT: std::time::Duration = std::time::Duration::from_secs(5);
/// How often the server looks for public catalogues whose snapshot is out of date.
const INDEX_EVERY: std::time::Duration = std::time::Duration::from_secs(600);
/// At most this many threads scan the snapshots.
const SCANNERS: usize = 8;
/// What the kept answers may weigh, in bytes of JSON; the least recent searches go first.
const KEPT_BYTES: usize = 64 << 20;
/// The folder under the storage folder that holds the snapshots: a cache, safe to delete.
const SNAPSHOTS: &str = "public-index";

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(events))
}

/// The answers kept, by search, and the snapshots being rewritten.
#[derive(Debug, Clone)]
pub struct PublicIndex {
    kept: Arc<Mutex<Kept>>,
    /// Each room whose snapshot is being rewritten, and what turns true when it is done.
    building: Arc<Mutex<HashMap<String, watch::Receiver<bool>>>>,
    /// One rewrite at a time.
    builds: Arc<Semaphore>,
}

impl Default for PublicIndex {
    fn default() -> Self {
        Self {
            kept: Arc::default(),
            building: Arc::default(),
            builds: Arc::new(Semaphore::new(1)),
        }
    }
}

#[derive(Debug, Default)]
struct Kept {
    clock: u64,
    searches: HashMap<String, Search>,
}

#[derive(Debug, Default)]
struct Search {
    used: u64,
    rooms: HashMap<String, Arc<Matches>>,
}

/// What a search found in one catalogue as its file was at `stamp`: how many, and the first
/// `MAX_LIMIT` by start time, as the JSON the answer sends.
#[derive(Debug)]
struct Matches {
    stamp: String,
    total: usize,
    first: Vec<Line>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Line {
    start: i64,
    stop: i64,
    json: Arc<str>,
}

impl Matches {
    fn bytes(&self) -> usize {
        self.first.iter().map(|line| line.json.len()).sum()
    }
}

impl PublicIndex {
    fn kept(&self, key: &str, room_id: &str, stamp: &str) -> Option<Arc<Matches>> {
        let mut kept = self.kept.lock().ok()?;
        kept.clock += 1;
        let clock = kept.clock;
        let search = kept.searches.get_mut(key)?;
        search.used = clock;
        let matches = search.rooms.get(room_id)?;
        (matches.stamp == stamp).then(|| Arc::clone(matches))
    }

    /// Rewrites the snapshot of `room_id` in the background, unless that is under way; what
    /// it answers turns true when the rewrite is done. The rewrite does not end with the
    /// search that asked for it.
    fn rebuild(&self, state: &AppState, room_id: &str) -> watch::Receiver<bool> {
        let Ok(mut building) = self.building.lock() else {
            return watch::channel(true).1;
        };
        if let Some(done) = building.get(room_id) {
            return done.clone();
        }
        let (sender, done) = watch::channel(false);
        building.insert(room_id.to_owned(), done.clone());
        let (index, state, room_id) = (self.clone(), state.clone(), room_id.to_owned());
        tokio::spawn(async move {
            let permit = index.builds.acquire().await;
            let stamp = file_stamp(&state, &room_id);
            if let Err(error) = snapshot(&state, &room_id, &stamp).await {
                tracing::warn!(room = %room_id, %error, "a public catalogue could not be indexed");
            }
            drop(permit);
            if let Ok(mut building) = index.building.lock() {
                building.remove(&room_id);
            }
            let _ = sender.send(true);
        });
        done
    }

    /// Keeps what was found, then forgets the catalogues that are no longer public and the
    /// least recent searches past the budget.
    fn keep(&self, key: &str, found: &[(String, Arc<Matches>)], public: &HashSet<&str>) {
        let Ok(mut kept) = self.kept.lock() else {
            return;
        };
        kept.clock += 1;
        let clock = kept.clock;
        let search = kept.searches.entry(key.to_owned()).or_default();
        search.used = clock;
        search
            .rooms
            .retain(|room_id, _| public.contains(room_id.as_str()));
        for (room_id, matches) in found {
            search.rooms.insert(room_id.clone(), Arc::clone(matches));
        }
        // simplify: weighs every kept answer on each keep, a few thousand catalogue entries.
        // Count the bytes as they come and go if many searches are kept.
        while weight(&kept.searches) > KEPT_BYTES {
            let oldest = kept
                .searches
                .iter()
                .min_by_key(|(_, search)| search.used)
                .map(|(key, _)| key.clone());
            match oldest {
                Some(oldest) => kept.searches.remove(&oldest),
                None => break,
            };
        }
    }
}

fn weight(searches: &HashMap<String, Search>) -> usize {
    searches
        .values()
        .flat_map(|search| search.rooms.values())
        .map(|matches| matches.bytes())
        .sum()
}

#[derive(Debug, Default, Deserialize)]
struct EventsQuery {
    q: Option<String>,
    /// The search as a rule's predicate in JSON, as a smart catalogue stores it.
    #[serde(rename = "where")]
    predicate: Option<String>,
    limit: Option<String>,
}

/// The first events found, in time order, and how many there are in all.
#[derive(Debug, Serialize, utoipa::ToSchema)]
struct Found {
    events: Vec<PublicEvent>,
    total: usize,
    /// How many public catalogues were left out: their index is still being written.
    indexing: usize,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct PublicEvent {
    /// The public catalogue that holds it: its id, as `/api/public` lists it.
    catalogue: String,
    event: EventRecord,
}

#[utoipa::path(
    get,
    path = "/api/public/events",
    params(
        ("q" = Option<String>, Query, description = "A search, as the search box takes it; `catalogue:\"…\"` names a public catalogue"),
        ("where" = Option<String>, Query, description = "Instead of `q`, the search as a rule's predicate in JSON, as a smart catalogue stores it"),
        ("limit" = Option<u32>, Query, minimum = 1, maximum = 10000, description = "How many events to return, 1000 by default and 10000 at most"),
    ),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The events found in the public catalogues; the ETag changes when one of them does", body = Found),
        (status = 304, description = "The answer the If-None-Match tag names still holds"),
        (status = 401, description = "No valid session", body = Detail),
        (status = 422, description = "The search does not read, or the limit is not a number", body = Detail),
    )
)]
async fn events(
    State(state): State<AppState>,
    CurrentUser(_): CurrentUser,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Result<Response, ApiError> {
    let limit = limit(query.limit.as_deref())?;
    let rooms = db::public_rooms(&state.pool).await?;
    let names: Vec<(String, String)> = rooms
        .iter()
        .map(|room| (room.id.clone(), room.name.clone()))
        .collect();
    let predicate = asked(&query, &names)?;
    lookups(&predicate)?;
    let snapshots = current_snapshots(&state, &names).await;
    let indexing = snapshots
        .iter()
        .filter(|(_, stamp)| stamp.is_none())
        .count();
    let stamps: Vec<(String, String)> = snapshots
        .into_iter()
        .filter_map(|(id, stamp)| Some((id, stamp?)))
        .collect();
    let key = serde_json::to_string(&predicate).map_err(|_| ApiError::Internal)?;
    let tag = HeaderValue::from_str(&version_tag(&key, limit, &stamps))
        .map_err(|_| ApiError::Internal)?;
    let held = headers.get(header::IF_NONE_MATCH) == Some(&tag);
    if held && !uses_now(&predicate) {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, tag)]).into_response());
    }
    let found = found(&state, &predicate, &key, &stamps).await?;
    let json = HeaderValue::from_static("application/json");
    Ok((
        [(header::CONTENT_TYPE, json), (header::ETAG, tag)],
        answer(&found, limit, indexing),
    )
        .into_response())
}

fn limit(text: Option<&str>) -> Result<usize, ApiError> {
    match text {
        None => Ok(DEFAULT_LIMIT),
        Some(text) => text
            .parse::<usize>()
            .ok()
            .filter(|limit| (1..=MAX_LIMIT).contains(limit))
            .ok_or_else(|| {
                ApiError::Unprocessable(format!("limit is a number from 1 to {MAX_LIMIT}"))
            }),
    }
}

/// The search `q` writes, or `where` gives as a predicate.
fn asked(query: &EventsQuery, names: &[(String, String)]) -> Result<Predicate, ApiError> {
    let unreadable = |error: String| ApiError::Unprocessable(error);
    let Some(predicate) = &query.predicate else {
        let text = query.q.as_deref().unwrap_or_default();
        return cocat_core::query::parse(text, names)
            .map_err(|error| unreadable(error.to_string()));
    };
    let predicate: serde_json::Value =
        serde_json::from_str(predicate).map_err(|error| unreadable(error.to_string()))?;
    let rule = serde_json::json!({"filter": {"public": true}, "where": predicate});
    match cocat_core::rules::parse(&rule).map_err(|error| unreadable(error.to_string()))? {
        cocat_core::rules::Set::Filter { predicate, .. } => Ok(predicate),
        _ => Err(ApiError::Internal),
    }
}

fn lookups(predicate: &Predicate) -> Result<Lookups, ApiError> {
    Lookups::new(predicate, Timestamp::now(), &mut |_| Ok(HashSet::new()))
        .map_err(|error| ApiError::Unprocessable(error.to_string()))
}

/// Names the answer: the search, the limit and every catalogue's file as it is now.
fn version_tag(key: &str, limit: usize, stamps: &[(String, String)]) -> String {
    let mut hash = Sha256::new();
    hash.update(key);
    hash.update(limit.to_string());
    for (room_id, stamp) in stamps {
        hash.update([0]);
        hash.update(room_id);
        hash.update([0]);
        hash.update(stamp);
    }
    format!("\"{:x}\"", hash.finalize())
}

/// A search with a time relative to now finds something else as time passes: never kept.
fn uses_now(predicate: &Predicate) -> bool {
    match predicate {
        Predicate::Cmp(_, _, value) => relative::is_relative(value),
        Predicate::Not(one) => uses_now(one),
        Predicate::And(all) | Predicate::Or(all) => all.iter().any(uses_now),
        _ => false,
    }
}

/// The stamp each catalogue's snapshot was written at, `None` while it has none. Snapshots
/// that are missing or out of date are rewritten in the background, and waited for at most
/// `WAIT`: a small catalogue's change shows at once, a big one answers as it was.
async fn current_snapshots(
    state: &AppState,
    rooms: &[(String, String)],
) -> Vec<(String, Option<String>)> {
    let read = || -> Vec<(String, Option<String>)> {
        rooms
            .iter()
            .map(|(id, _)| (id.clone(), snapshot_stamp(&snapshot_path(state, id))))
            .collect()
    };
    let first = read();
    let index = state.rooms.public_index();
    let rewriting: Vec<_> = first
        .iter()
        .filter(|(room_id, stamp)| stamp.as_deref() != Some(file_stamp(state, room_id).as_str()))
        .map(|(room_id, _)| index.rebuild(state, room_id))
        .collect();
    if rewriting.is_empty() {
        return first;
    }
    let written = rewriting.into_iter().map(|mut done| async move {
        let _ = done.wait_for(|done| *done).await;
    });
    let _ = tokio::time::timeout(WAIT, join_all(written)).await;
    read()
}

/// Looks for out-of-date snapshots when the server starts, then every `INDEX_EVERY`, so the
/// first search after a deploy or a relay refresh rarely waits.
pub fn spawn_indexer(state: AppState) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(INDEX_EVERY);
        loop {
            ticker.tick().await;
            match db::public_rooms(&state.pool).await {
                Ok(rooms) => {
                    let names: Vec<(String, String)> =
                        rooms.into_iter().map(|room| (room.id, room.name)).collect();
                    for (room_id, stamp) in names.iter().map(|(id, _)| (id, file_stamp(&state, id)))
                    {
                        if snapshot_stamp(&snapshot_path(&state, room_id)).as_deref()
                            != Some(stamp.as_str())
                        {
                            state.rooms.public_index().rebuild(&state, room_id);
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "the public catalogues could not be listed for indexing")
                }
            }
        }
    })
}

fn snapshot_path(state: &AppState, room_id: &str) -> PathBuf {
    state
        .rooms
        .update_dir()
        .join(SNAPSHOTS)
        .join(format!("{room_id}.jsonl"))
}

/// What the search finds in each catalogue: kept, or scanned from its snapshot as `stamps`
/// names it.
async fn found(
    state: &AppState,
    predicate: &Predicate,
    key: &str,
    stamps: &[(String, String)],
) -> Result<Vec<(String, Arc<Matches>)>, ApiError> {
    let index = state.rooms.public_index();
    let keeps = !uses_now(predicate);
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for (room_id, stamp) in stamps {
        match index.kept(key, room_id, stamp).filter(|_| keeps) {
            Some(matches) => found.push((room_id.clone(), matches)),
            None => missing.push((room_id.clone(), snapshot_path(state, room_id))),
        }
    }
    let predicate = predicate.clone();
    let scanned = tokio::task::spawn_blocking(move || scan_all(&missing, &predicate))
        .await
        .map_err(|_| ApiError::Internal)??;
    if keeps {
        let public = stamps.iter().map(|(id, _)| id.as_str()).collect();
        index.keep(key, &scanned, &public);
    }
    found.extend(scanned);
    Ok(found)
}

#[derive(Debug, thiserror::Error)]
enum SnapshotError {
    #[error(transparent)]
    Room(#[from] crate::room::RoomError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// The snapshot file of `room_id` as its room file is at `stamp`: its first line names the
/// stamp, each other line is one live event.
async fn snapshot(state: &AppState, room_id: &str, stamp: &str) -> Result<PathBuf, SnapshotError> {
    let folder = state.rooms.update_dir().join(SNAPSHOTS);
    let path = snapshot_path(state, room_id);
    if snapshot_stamp(&path).as_deref() == Some(stamp) {
        return Ok(path);
    }
    let header = Header {
        stamp: stamp.to_owned(),
    };
    // The text while the room is loaded: events as structs weigh five times their JSON.
    let text = state
        .rooms
        .read(room_id, move |doc| snapshot_text(doc, &header))
        .await??;
    let written = path.clone();
    tokio::task::spawn_blocking(move || write_snapshot(&folder, &written, &text))
        .await
        .map_err(std::io::Error::other)??;
    Ok(path)
}

fn snapshot_text(doc: &yrs::Doc, header: &Header) -> std::io::Result<Vec<u8>> {
    let everything = Predicate::And(Vec::new());
    let events = model::search(&doc.transact(), &everything, Timestamp::now()).unwrap_or_default();
    let mut text = serde_json::to_vec(header)?;
    for event in &events {
        text.push(b'\n');
        serde_json::to_writer(&mut text, event)?;
    }
    Ok(text)
}

#[derive(Serialize, Deserialize)]
struct Header {
    stamp: String,
}

fn snapshot_stamp(path: &Path) -> Option<String> {
    use std::io::BufRead as _;
    let file = std::fs::File::open(path).ok()?;
    let line = std::io::BufReader::new(file).lines().next()?.ok()?;
    serde_json::from_str::<Header>(&line)
        .ok()
        .map(|header| header.stamp)
}

/// Written beside and renamed over, so a scan never reads half a file.
fn write_snapshot(folder: &Path, path: &Path, text: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(folder)?;
    let partial = folder.join(format!(".{}.partial", uuid::Uuid::new_v4().simple()));
    let mut file = std::fs::File::create(&partial)?;
    file.write_all(text)?;
    file.sync_all()?;
    std::fs::rename(&partial, path)
}

/// Scans the snapshots on a few threads.
fn scan_all(
    snapshots: &[(String, PathBuf)],
    predicate: &Predicate,
) -> Result<Vec<(String, Arc<Matches>)>, ApiError> {
    let lookups = lookups(predicate)?;
    let next = AtomicUsize::new(0);
    let found = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism()
        .map_or(1, |count| count.get())
        .min(SCANNERS)
        .min(snapshots.len().max(1));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                while let Some((room_id, path)) =
                    snapshots.get(next.fetch_add(1, Ordering::Relaxed))
                {
                    match scan(path, &in_room(predicate, room_id), &lookups) {
                        Ok(matches) => {
                            if let Ok(mut found) = found.lock() {
                                found.push((room_id.clone(), Arc::new(matches)));
                            }
                        }
                        Err(error) => {
                            tracing::warn!(room = %room_id, %error, "a public catalogue was left out of a search");
                        }
                    }
                }
            });
        }
    });
    found.into_inner().map_err(|_| ApiError::Internal)
}

/// One snapshot, its stamp read from its own first line: it may have been rewritten since
/// the search looked.
fn scan(path: &Path, predicate: &Predicate, lookups: &Lookups) -> std::io::Result<Matches> {
    let text = std::fs::read_to_string(path)?;
    let mut lines = text.lines();
    let stamp = lines
        .next()
        .and_then(|line| serde_json::from_str::<Header>(line).ok())
        .map(|header| header.stamp)
        .ok_or_else(|| std::io::Error::other("a snapshot without its stamp"))?;
    let mut first: Vec<Line> = lines
        .filter_map(|line| Some((line, serde_json::from_str::<EventRecord>(line).ok()?)))
        .filter(|(_, event)| model::record_matches(predicate, lookups, event))
        .map(|(line, event)| Line {
            start: event.start.unix_nanos().unwrap_or_default(),
            stop: event.stop.unix_nanos().unwrap_or_default(),
            json: Arc::from(line),
        })
        .collect();
    let total = first.len();
    first.sort();
    first.truncate(MAX_LIMIT);
    Ok(Matches {
        stamp,
        total,
        first,
    })
}

/// The answer's JSON, the events written as their snapshot holds them; `indexing` counts the
/// catalogues not searched yet.
fn answer(found: &[(String, Arc<Matches>)], limit: usize, indexing: usize) -> String {
    let total: usize = found.iter().map(|(_, matches)| matches.total).sum();
    let mut lines: Vec<(&Line, &str)> = found
        .iter()
        .flat_map(|(room_id, matches)| {
            matches
                .first
                .iter()
                .map(move |line| (line, room_id.as_str()))
        })
        .collect();
    lines.sort();
    let events: Vec<String> = lines
        .into_iter()
        .take(limit)
        .map(|(line, room_id)| {
            let catalogue = serde_json::Value::from(room_id).to_string();
            format!("{{\"catalogue\":{catalogue},\"event\":{}}}", line.json)
        })
        .collect();
    format!(
        "{{\"total\":{total},\"indexing\":{indexing},\"events\":[{}]}}",
        events.join(",")
    )
}

/// `predicate` for the events of one public catalogue: `catalogue:"…"` asks whether it is
/// that catalogue, and every public event is listed by its catalogue.
fn in_room(predicate: &Predicate, room_id: &str) -> Predicate {
    let always = |yes: bool| match yes {
        true => Predicate::And(Vec::new()),
        false => Predicate::Or(Vec::new()),
    };
    let inner = |all: &[Predicate]| all.iter().map(|p| in_room(p, room_id)).collect();
    match predicate {
        Predicate::InCatalogue(catalogue) => always(catalogue.as_deref() == Some(room_id)),
        Predicate::And(all) => Predicate::And(inner(all)),
        Predicate::Or(any) => Predicate::Or(inner(any)),
        Predicate::Not(one) => Predicate::Not(Box::new(in_room(one, room_id))),
        other => other.clone(),
    }
}
