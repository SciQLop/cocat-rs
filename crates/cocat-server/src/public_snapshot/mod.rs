//! `/api/public/snapshot`: the public catalogues as Parquet files that a browser queries
//! with SQL (docs/design/browser-sql-analysis.md). Every signed-in account reads them, as it
//! reads `/api/public/events`.
//!
//! A snapshot is a manifest and the files it names. Each file's name carries a hash of its
//! bytes, so a browser keeps it forever; the manifest changes when a catalogue does, soon
//! after: a request for it starts the rewrite when the catalogues changed. The
//! files are written from each catalogue's JSON-lines file, which the public search keeps
//! up to date: no room is loaded for them.

mod write;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::task::JoinHandle;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::api::{ApiError, ApiPath, CurrentUser};
use crate::openapi::Detail;
use crate::public_api::file_stamp;
use crate::relay::store;

/// The folder under the storage folder that holds the snapshots: safe to delete.
const FOLDER: &str = "public-snapshot";
const MANIFEST: &str = "manifest.json";
/// The manifest before the current one: its files stay, for a page that loaded it.
const PREVIOUS: &str = "manifest.previous.json";
/// Changes when the files' layout does, so an unchanged server still rewrites them.
const LAYOUT: &str = "3";
/// How often the server rewrites the snapshot, when a catalogue changed.
const REFRESH_EVERY: std::time::Duration = std::time::Duration::from_secs(3600);
const NOT_A_FILE: &str = "No such snapshot file";
const IMMUTABLE: &str = "private, max-age=31536000, immutable";
/// The survey's method, which the browser and the survey notebook both run on the files.
const SURVEY: &str = include_str!("../../sql/survey.sql");

/// One snapshot is written at a time, whichever server state asks.
static WRITING: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(Default::default);

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(manifest))
        .routes(routes!(file))
}

/// The files of a snapshot, by the name a query uses for them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Manifest {
    /// Changes whenever a file does.
    pub version: String,
    /// When it was written (RFC 3339).
    pub created: String,
    /// `events.parquet`, `catalogues.parquet`, `attributes/<catalogue id>.parquet` and
    /// `survey.sql`.
    pub files: BTreeMap<String, FileEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct FileEntry {
    /// The name to fetch it by, under `/api/public/snapshot/`.
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error(transparent)]
    Db(#[from] crate::db::DbError),
    #[error(transparent)]
    Index(#[from] crate::public_search::SnapshotError),
    #[error(transparent)]
    Write(#[from] write::WriteError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[utoipa::path(
    get,
    path = "/api/public/snapshot",
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The files of the current snapshot of the public catalogues", body = Manifest),
        (status = 304, description = "The snapshot the If-None-Match tag names is still current"),
        (status = 401, description = "No valid session", body = Detail),
    )
)]
async fn manifest(
    State(state): State<AppState>,
    CurrentUser(_): CurrentUser,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let manifest = match read_manifest(&folder(&state), MANIFEST) {
        Some(manifest) => {
            refresh_if_changed(&state, &manifest.version).await;
            manifest
        }
        None => refresh(&state).await.map_err(|error| {
            tracing::warn!(%error, "the public snapshot could not be written");
            ApiError::Internal
        })?,
    };
    let tag = HeaderValue::from_str(&format!("\"{}\"", manifest.version))
        .map_err(|_| ApiError::Internal)?;
    let cache = (header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    if headers.get(header::IF_NONE_MATCH) == Some(&tag) {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, tag), cache]).into_response());
    }
    Ok(([(header::ETAG, tag), cache], axum::Json(manifest)).into_response())
}

#[utoipa::path(
    get,
    path = "/api/public/snapshot/{name}",
    params(("name" = String, Path, description = "A file's name as the manifest gives it")),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The file; its name never names other bytes"),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such file", body = Detail),
    )
)]
async fn file(
    State(state): State<AppState>,
    CurrentUser(_): CurrentUser,
    ApiPath(name): ApiPath<String>,
) -> Result<Response, ApiError> {
    let not_found = || ApiError::NotFound(NOT_A_FILE.to_owned());
    if !is_file_name(&name) {
        return Err(not_found());
    }
    let bytes = tokio::fs::read(folder(&state).join(&name))
        .await
        .map_err(|_| not_found())?;
    let kind = match name.rsplit_once('.').map(|(_, ext)| ext) {
        Some("parquet") => "application/vnd.apache.parquet",
        _ => "text/plain; charset=utf-8",
    };
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(kind)),
            (header::CACHE_CONTROL, HeaderValue::from_static(IMMUTABLE)),
        ],
        bytes,
    )
        .into_response())
}

/// A hashed file name, `<stem>-<16 hex>.<ext>`: never the manifest, a partial file or a
/// path.
fn is_file_name(name: &str) -> bool {
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    let Some((_, hash)) = stem.rsplit_once('-') else {
        return false;
    };
    matches!(ext, "parquet" | "sql")
        && hash.len() == 16
        && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn folder(state: &AppState) -> PathBuf {
    state.rooms.update_dir().join(FOLDER)
}

fn read_manifest(folder: &Path, name: &str) -> Option<Manifest> {
    serde_json::from_slice(&std::fs::read(folder.join(name)).ok()?).ok()
}

/// Writes a new snapshot when a catalogue changed since the last one; the current one
/// otherwise.
pub async fn refresh(state: &AppState) -> Result<Manifest, SnapshotError> {
    let _writing = WRITING.lock().await;
    let (version, catalogues) = wanted(state).await?;
    let folder = folder(state);
    if let Some(current) = read_manifest(&folder, MANIFEST).filter(|m| m.version == version) {
        return Ok(current);
    }
    let mut paths = Vec::new();
    for catalogue in &catalogues {
        paths.push(crate::public_search::current_snapshot(state, &catalogue.id).await?);
    }
    let manifest =
        tokio::task::spawn_blocking(move || publish(&folder, version, &catalogues, &paths))
            .await
            .map_err(std::io::Error::other)??;
    Ok(manifest)
}

/// Starts writing a new snapshot in the background when the catalogues changed since
/// `version`, unless one is being written. The visitor gets the current one meanwhile.
async fn refresh_if_changed(state: &AppState, version: &str) {
    let changed = match wanted(state).await {
        Ok((wanted, _)) => wanted != version,
        Err(error) => {
            tracing::warn!(%error, "the public catalogues could not be listed");
            false
        }
    };
    if changed && WRITING.try_lock().is_ok() {
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = refresh(&state).await {
                tracing::warn!(%error, "the public snapshot could not be written");
            }
        });
    }
}

/// The catalogues a snapshot written now would hold, and its version: a hash of the layout,
/// survey.sql, and each catalogue's room file stamp and index entry. Cheap: no room is read.
async fn wanted(state: &AppState) -> Result<(String, Vec<write::Catalogue>), SnapshotError> {
    let mut catalogues = Vec::new();
    let mut version = Sha256::new();
    version.update(LAYOUT);
    version.update(SURVEY);
    for (room, relay) in store::public_index(&state.pool).await? {
        let stamp = file_stamp(state, &room.id);
        let catalogue = write::Catalogue {
            id: room.id,
            name: room.name,
            folder: room.folder,
            source: room.source,
            description: room.description,
            relay_source: relay.as_ref().map(|relay| relay.source.clone()),
            relay_url: relay.as_ref().and_then(|relay| relay.url.clone()),
            relay_published: relay.as_ref().map(|relay| relay.published),
        };
        version.update(serde_json::to_vec(&(
            &catalogue.id,
            &stamp,
            &catalogue.name,
            &catalogue.folder,
            &catalogue.source,
            &catalogue.description,
            (
                &catalogue.relay_source,
                &catalogue.relay_url,
                catalogue.relay_published,
            ),
        ))?);
        catalogues.push(catalogue);
    }
    Ok((
        format!("{:x}", version.finalize())[..16].to_owned(),
        catalogues,
    ))
}

/// Writes the files, then the manifest over the last one, then drops the files neither
/// names.
fn publish(
    folder: &Path,
    version: String,
    catalogues: &[write::Catalogue],
    paths: &[PathBuf],
) -> Result<Manifest, SnapshotError> {
    std::fs::create_dir_all(folder)?;
    let events: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    let mut files = write::snapshot(catalogues, &events, &|file| hashed_name(file).0)?;
    files.push(write::Written {
        logical: "survey.sql".into(),
        stem: "survey".into(),
        bytes: SURVEY.as_bytes().to_vec(),
    });
    let mut entries = BTreeMap::new();
    for file in &files {
        let (name, sha256) = hashed_name(file);
        write_atomically(folder, &name, &file.bytes)?;
        entries.insert(
            file.logical.clone(),
            FileEntry {
                name,
                bytes: file.bytes.len() as u64,
                sha256,
            },
        );
    }
    let manifest = Manifest {
        version,
        created: cocat_core::time::Timestamp::now()
            .rfc3339()
            .unwrap_or_default(),
        files: entries,
    };
    if folder.join(MANIFEST).exists() {
        std::fs::rename(folder.join(MANIFEST), folder.join(PREVIOUS))?;
    }
    write_atomically(folder, MANIFEST, &serde_json::to_vec_pretty(&manifest)?)?;
    prune(folder)?;
    Ok(manifest)
}

/// `<stem>-<first 16 hex of its sha256>.<ext>`, and the whole hash.
fn hashed_name(file: &write::Written) -> (String, String) {
    let sha256 = format!("{:x}", Sha256::digest(&file.bytes));
    let ext = file.logical.rsplit_once('.').map_or("bin", |(_, ext)| ext);
    (format!("{}-{}.{ext}", file.stem, &sha256[..16]), sha256)
}

/// Written beside and renamed over, so a reader never gets half a file.
fn write_atomically(folder: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    let partial = folder.join(format!(".{}.partial", uuid::Uuid::new_v4().simple()));
    std::fs::write(&partial, bytes)?;
    std::fs::rename(partial, folder.join(name))
}

fn prune(folder: &Path) -> std::io::Result<()> {
    let kept: std::collections::HashSet<String> = [MANIFEST, PREVIOUS]
        .into_iter()
        .filter_map(|name| read_manifest(folder, name))
        .flat_map(|manifest| manifest.files.into_values().map(|file| file.name))
        .collect();
    for entry in std::fs::read_dir(folder)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if is_file_name(&name) && !kept.contains(&name) {
            std::fs::remove_file(folder.join(&name))?;
        }
    }
    Ok(())
}

/// Rewrites the snapshot when the server starts, then every `REFRESH_EVERY`, when a
/// catalogue or the survey's method changed.
pub fn spawn_refresher(state: AppState) -> JoinHandle<()> {
    tokio::spawn(async move {
        // At start too: a new release may bring a new survey.sql. The catalogues' own files
        // are rewritten through the indexer's queue, so this never loads a room twice.
        let mut ticker = tokio::time::interval(REFRESH_EVERY);
        loop {
            ticker.tick().await;
            if let Err(error) = refresh(&state).await {
                tracing::warn!(%error, "the public snapshot could not be written");
            }
        }
    })
}
