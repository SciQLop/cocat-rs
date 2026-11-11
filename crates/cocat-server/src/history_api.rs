//! A room's version history under `/api`: the versions, what each changed, the room as it
//! was, and the names people give versions.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::{Range, RangeInclusive};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use cocat_core::model;
use cocat_core::restore::{document_json, restore_document};
use cocat_core::role::Role;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::macros::format_description;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;
use yrs::{Doc, ReadTxn, StateVector, Transact};

use crate::api::{self, ApiError, ApiPath, CurrentUser, JsonBody, membership};
use crate::catalogues_api::{cursor, page_limit};
use crate::copy_chain::{Source, record_copy};
use crate::db::{DocumentKind, RoomRow};
use crate::history::diff::{
    Analysis, CatalogueChange, Counts, EventChange, analyse, before_and_after, members, page,
    replay_until,
};
use crate::history::versions::{Saved, Version, span, spans};
use crate::openapi::Detail;
use crate::relay::store;
use crate::room::{Author, RoomError};
use crate::{AppState, access, db, documents, lifecycle, presence};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list))
        .routes(routes!(changes))
        .routes(routes!(state_at))
        .routes(routes!(name, unname))
        .routes(routes!(restore))
        .routes(routes!(commit))
        .routes(routes!(copy))
}

#[derive(Debug, Default, Deserialize)]
struct ChangesQuery {
    catalogue: Option<String>,
    since: Option<u64>,
    document: Option<Uuid>,
    /// Strings, so that a value that does not parse is the 422, not axum's 400.
    limit: Option<String>,
    cursor: Option<String>,
}

/// What the version changed: the counts, every catalogue change, and the event changes, all
/// of them or one page.
#[derive(Debug, Serialize, utoipa::ToSchema)]
struct Changes<'a> {
    counts: &'a Counts,
    catalogues: &'a [CatalogueChange],
    #[schema(value_type = Vec<EventChange>)]
    events: Cow<'a, [EventChange]>,
    /// With a `limit`, the cursor of the next page; none on the last.
    next: Option<String>,
}

/// The page asked for: its size and where the previous one stopped.
struct Paging {
    limit: usize,
    after: Option<(i128, String)>,
}

/// A cursor without a size is refused.
fn paging(query: &ChangesQuery) -> Result<Option<Paging>, ApiError> {
    let after = query.cursor.as_deref().map(cursor).transpose()?;
    match query.limit.as_deref() {
        Some(text) => Ok(Some(Paging {
            limit: page_limit(text)?,
            after,
        })),
        None if after.is_some() => Err(unprocessable("A cursor needs a limit")),
        None => Ok(None),
    }
}

/// The records a diff replays: the version's own, or, with `since`, those from the end of the
/// older version to the end of the newer, whichever order they are given in.
fn compared(
    version: Range<usize>,
    since: Option<u64>,
    span_of: impl Fn(u64) -> Option<Range<usize>>,
) -> Option<(usize, usize)> {
    let Some(since) = since else {
        return Some((version.start, version.end));
    };
    let other = span_of(since)?;
    Some((version.end.min(other.end), version.end.max(other.end)))
}

#[derive(Debug, Default, Deserialize)]
struct HistoryQuery {
    catalogue: Option<String>,
    event: Option<String>,
    document: Option<Uuid>,
}

#[derive(Debug, Default, Deserialize)]
struct DocumentQuery {
    document: Option<Uuid>,
}

/// The key whose log the history reads, the project's catalogues or one of its documents
/// or proposals, with a document's kind and the caller's role on it. A proposal the caller
/// may not read is missing, as an unknown document is.
async fn history_key(
    state: &AppState,
    room_id: &str,
    document: Option<Uuid>,
    user: Uuid,
) -> Result<(String, Option<DocumentKind>, Role), ApiError> {
    let missing = || ApiError::NotFound("No such document".to_owned());
    let project_role = membership(state, room_id, user).await?;
    let content = access::content(&state.pool, room_id, document)
        .await?
        .ok_or_else(missing)?;
    let role = access::role_on(&state.pool, &content, project_role, user)
        .await?
        .ok_or_else(missing)?;
    let key = document.map_or_else(|| room_id.to_owned(), |doc| documents::key(room_id, doc));
    Ok((key, content.kind(), role))
}

fn unprocessable(detail: &str) -> ApiError {
    ApiError::Unprocessable(detail.to_owned())
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct VersionView {
    #[serde(flatten)]
    version: Version,
    author_email: Option<String>,
    /// The colour the author shows in presence.
    author_color: Option<&'static str>,
    /// The name someone gave the version, else the one its import or materialization gave it.
    name: Option<String>,
    /// Someone saved the version on purpose: it ends where they saved it.
    committed: bool,
    /// The note they saved it with.
    message: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct History {
    versions: Vec<VersionView>,
}

/// 1 to 100 characters once trimmed; anything else is the schema's 422.
#[derive(Debug, Deserialize)]
#[serde(try_from = "String")]
struct VersionName(String);

impl TryFrom<String> for VersionName {
    type Error = &'static str;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        let name = name.trim();
        match name.chars().count() {
            1..=100 => Ok(Self(name.to_owned())),
            _ => Err("the name must be 1 to 100 characters"),
        }
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct NameBody {
    #[schema(value_type = String, min_length = 1, max_length = 100)]
    name: VersionName,
}

#[utoipa::path(
    get,
    path = "/api/rooms/{room_id}/history",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("catalogue" = Option<String>, Query, description = "Only the versions that touched this catalogue or its members' events; for a smart catalogue, any event"),
        ("event" = Option<String>, Query, description = "Only the versions that added, changed or removed this event"),
        ("document" = Option<String>, Query, description = "A workspace document's versions instead of the catalogues'"),
    ),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The versions, newest first", body = History),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such room or document, or the caller is not a member", body = Detail),
        (status = 422, description = "A catalogue or event filter together with a document", body = Detail),
    )
)]
async fn list(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<History>, ApiError> {
    let role = membership(&state, &room_id, user.id).await?;
    let filtered = query.catalogue.is_some() || query.event.is_some();
    if filtered && query.document.is_some() {
        return Err(unprocessable(
            "Catalogue and event filters apply to catalogues only",
        ));
    }
    let (key, _, _) = history_key(&state, &room_id, query.document, user.id).await?;
    let filter = match query.catalogue {
        Some(uuid) => Some(Filter::new(&state, &room_id, uuid).await?),
        None => None,
    };
    let mut versions = room_versions(&state, &key).await?;
    if let Some(filter) = filter {
        versions.retain(|version| filter.keeps(version));
        for version in &mut versions {
            version.count_only(&filter.members);
        }
    }
    if let Some(event) = query.event {
        versions.retain(|version| version.events.contains(&event));
    }
    let versions = views(&state, &key, role, versions).await?;
    Ok(Json(History { versions }))
}

#[utoipa::path(
    get,
    path = "/api/rooms/{room_id}/history/{version}",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("version" = u64, Path, description = "Version id: its first record"),
        ("catalogue" = Option<String>, Query, description = "Only this catalogue and its members"),
        ("since" = Option<u64>, Query, description = "Another version: what changed from the end of the older of the two to the end of the newer"),
        ("document" = Option<String>, Query, description = "A workspace document: refused, changes are listed for catalogues only"),
        ("limit" = Option<String>, Query, description = "Page size, 1 to 10000: answer only this many event changes, in order of start then uuid; without it, every change"),
        ("cursor" = Option<String>, Query, min_length = 1, description = "The `next` of the previous page; needs a limit"),
    ),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "What the version changed, or what changed between two versions", body = Changes),
        (status = 422, description = "A limit out of range, or a cursor that is not valid or has no limit", body = Detail),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such room, version or membership", body = Detail),
        (status = 409, description = "A document was named: changes are listed for catalogues only", body = Detail),
    )
)]
async fn changes(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, version)): ApiPath<(String, u64)>,
    Query(query): Query<ChangesQuery>,
) -> Result<Response, ApiError> {
    membership(&state, &room_id, user.id).await?;
    if query.document.is_some() {
        return Err(ApiError::Conflict(
            "Changes are listed for catalogues only".to_owned(),
        ));
    }
    let paging = paging(&query)?;
    let saved = saved(&state, &room_id).await?;
    let cache = state.rooms.diff_cache().clone();
    let key_room = room_id.clone();
    let analysis = state
        .rooms
        .read_log(&room_id, 2, move |records, slots| {
            let (start, end) = compared(span(slots, &saved, version)?, query.since, |since| {
                span(slots, &saved, since)
            })?;
            // simplify: a cache hit still reads the whole log; key the cache by record count
            // and skip the read when a page of a version that cannot grow is asked again.
            let key = (key_room, start, end, query.catalogue.clone());
            Some(cache.get_or_compute(key, || {
                let (before, after) = before_and_after(records, start, end);
                analyse(&before, &after, query.catalogue.as_deref())
            }))
        })
        .await?
        .ok_or_else(no_such_version)?;
    Ok(Json(answer(&analysis, paging)).into_response())
}

fn answer(analysis: &Analysis, paging: Option<Paging>) -> Changes<'_> {
    let (events, next) = match paging {
        Some(Paging { limit, after }) => {
            let (page, next) = page(&analysis.diff, after, limit);
            (Cow::Owned(page), next)
        }
        None => (Cow::Borrowed(analysis.diff.events.as_slice()), None),
    };
    Changes {
        counts: &analysis.counts,
        catalogues: &analysis.diff.catalogues,
        events,
        next,
    }
}

#[utoipa::path(
    get,
    path = "/api/rooms/{room_id}/history/{version}/state",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("version" = u64, Path, description = "Version id: its first record"),
        ("document" = Option<String>, Query, description = "A workspace document instead of the catalogues"),
    ),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The room as of the version, as one Yjs v1 update; a document as the JSON of its root types, by name", content((Vec<u8> = "application/octet-stream"), (Object = "application/json"))),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such room, version or membership", body = Detail),
    )
)]
async fn state_at(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, version)): ApiPath<(String, u64)>,
    Query(query): Query<DocumentQuery>,
) -> Result<Response, ApiError> {
    let (key, _, _) = history_key(&state, &room_id, query.document, user.id).await?;
    if query.document.is_none() {
        let update = past(&state, &key, version, state_until).await?;
        let binary = [(header::CONTENT_TYPE, "application/octet-stream")];
        return Ok((binary, update).into_response());
    }
    let json = past(&state, &key, version, json_until).await?;
    let json = json.map_err(|error| {
        tracing::warn!(%error, key, "a document's past state is not JSON");
        ApiError::Internal
    })?;
    Ok(Json(json).into_response())
}

/// `read` of the log under `key` up to the end of the version.
async fn past<T: Send + 'static>(
    state: &AppState,
    key: &str,
    version: u64,
    read: fn(&[Bytes], usize) -> T,
) -> Result<T, ApiError> {
    let saved = saved(state, key).await?;
    state
        .rooms
        .read_log(key, 1, move |records, slots| {
            Some(read(records, span(slots, &saved, version)?.end))
        })
        .await?
        .ok_or_else(no_such_version)
}

/// A document as of the version ending before record `end`, as its root types' JSON.
fn json_until(records: &[Bytes], end: usize) -> Result<serde_json::Value, serde_json::Error> {
    document_json(&replay_until(&records[..end], u64::MAX))
}

/// The room as of the version ending before record `end`, as one update.
fn state_until(records: &[Bytes], end: usize) -> Vec<u8> {
    encoded(&replay_until(&records[..end], u64::MAX))
}

pub(crate) fn encoded(doc: &Doc) -> Vec<u8> {
    doc.transact()
        .encode_state_as_update_v1(&StateVector::default())
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct Copied {
    /// The new project's id.
    room: String,
    name: String,
}

#[utoipa::path(
    post,
    path = "/api/rooms/{room_id}/history/{version}/copy",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("version" = u64, Path, description = "Version id: its first record"),
    ),
    request_body = api::NameBody,
    security(("cookie" = [])),
    responses(
        (status = 201, description = "A new project, owned by the caller, holding the version as its first change", body = Copied),
        (status = 400, description = "The name has control characters", body = Detail),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such room, version or membership", body = Detail),
        (status = 409, description = "The copy does not fit in the caller's storage quota", body = Detail),
        (status = 422, description = "The name is empty or too long", body = Detail),
    )
)]
async fn copy(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, version)): ApiPath<(String, u64)>,
    JsonBody(body): JsonBody<api::NameBody>,
) -> Result<(StatusCode, Json<Copied>), ApiError> {
    membership(&state, &room_id, user.id).await?;
    let saved = saved(&state, &room_id).await?;
    let original = db::room_name(&state.pool, &room_id)
        .await?
        .unwrap_or_default();
    let source = (room_id.clone(), original.clone(), user.email.clone());
    let (update, started_ms) = state
        .rooms
        .read_log(&room_id, 1, move |records, slots| {
            let span = span(slots, &saved, version)?;
            let started_ms = slots
                .get(span.start)
                .copied()
                .flatten()
                .map(|stamp| stamp.at_ms);
            let doc = replay_until(&records[..span.end], u64::MAX);
            let (project, project_name, by) = &source;
            let from = Source {
                project,
                project_name,
                version: Some(version),
                by,
            };
            record_copy(&doc, &from);
            Some((encoded(&doc), started_ms))
        })
        .await?
        .ok_or_else(no_such_version)?;
    let first = copy_name(&original, started_ms);
    let making = make_copy(state, user.id, body.name.0, update, first);
    let room = tokio::spawn(making).await.map_err(|error| {
        tracing::warn!(%error, "a copy stopped half way");
        ApiError::Internal
    })??;
    let copied = Copied {
        room: room.id,
        name: room.name,
    };
    Ok((StatusCode::CREATED, Json(copied)))
}

/// Runs in its own task: a client that hangs up cannot cut it between the new project and
/// its content. Any failure after the project exists removes it.
pub(crate) async fn make_copy(
    state: AppState,
    user: Uuid,
    name: String,
    update: Vec<u8>,
    first: String,
) -> Result<RoomRow, ApiError> {
    let room = lifecycle::create_room(&state, user, &name).await?;
    let Err(error) = fill_copy(&state, &room.id, user, update, &first).await else {
        return Ok(room);
    };
    if let Err(cleanup) = lifecycle::delete_room(&state, &room.id, user).await {
        tracing::warn!(error = %cleanup, room = %room.id, "cannot remove a failed copy");
    }
    Err(error)
}

async fn fill_copy(
    state: &AppState,
    room_id: &str,
    user: Uuid,
    update: Vec<u8>,
    first: &str,
) -> Result<(), ApiError> {
    write_first_version(state, room_id, user, update).await?;
    db::name_version(&state.pool, room_id, 0..=0, first, user).await?;
    Ok(())
}

async fn write_first_version(
    state: &AppState,
    room_id: &str,
    user: Uuid,
    update: Vec<u8>,
) -> Result<(), ApiError> {
    let allowance = state.rooms.allowance(&state.pool, room_id).await?;
    state
        .rooms
        .write_as(room_id, allowance, Author::Client(user), update)
        .await?;
    Ok(())
}

/// "Copied from <project>, <date the version started>", cut to a version name's 100
/// characters.
pub(crate) fn copy_name(project: &str, started_ms: Option<i64>) -> String {
    let date = started_ms.and_then(utc_minute);
    let name = match date {
        Some(date) => format!("Copied from {project}, {date}"),
        None => format!("Copied from {project}"),
    };
    name.chars().take(100).collect()
}

fn utc_minute(ms: i64) -> Option<String> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .ok()?
        .format(format_description!(
            "[year]-[month]-[day] [hour]:[minute] UTC"
        ))
        .ok()
}

#[utoipa::path(
    put,
    path = "/api/rooms/{room_id}/history/{version}/name",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("version" = u64, Path, description = "Version id: its first record"),
    ),
    request_body = NameBody,
    security(("cookie" = [])),
    responses(
        (status = 204, description = "The version has the name"),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller cannot write to the room", body = Detail),
        (status = 404, description = "No such room, version or membership", body = Detail),
        (status = 422, description = "The name is blank or longer than 100 characters, or the body names a document: versions of catalogues only", body = Detail),
    )
)]
async fn name(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, version)): ApiPath<(String, u64)>,
    JsonBody(body): JsonBody<NameBody>,
) -> Result<StatusCode, ApiError> {
    let records = writable_version(&state, &room_id, user.id, version).await?;
    db::name_version(&state.pool, &room_id, records, &body.name.0, user.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/api/rooms/{room_id}/history/{version}/name",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("version" = u64, Path, description = "Version id: its first record"),
    ),
    security(("cookie" = [])),
    responses(
        (status = 204, description = "The version has no name"),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller cannot write to the room", body = Detail),
        (status = 404, description = "No such room, version or membership", body = Detail),
    )
)]
async fn unname(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, version)): ApiPath<(String, u64)>,
) -> Result<StatusCode, ApiError> {
    let records = writable_version(&state, &room_id, user.id, version).await?;
    db::unname_version(&state.pool, &room_id, records).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Up to 2000 characters once trimmed; blank is no note.
#[derive(Debug, Deserialize)]
#[serde(try_from = "String")]
struct Note(Option<String>);

impl TryFrom<String> for Note {
    type Error = &'static str;

    fn try_from(note: String) -> Result<Self, Self::Error> {
        let note = note.trim();
        match note.chars().count() {
            0 => Ok(Self(None)),
            1..=2000 => Ok(Self(Some(note.to_owned()))),
            _ => Err("the note must be at most 2000 characters"),
        }
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct CommitBody {
    #[serde(default)]
    #[schema(value_type = Option<String>, min_length = 1, max_length = 100)]
    name: Option<VersionName>,
    #[serde(default)]
    #[schema(value_type = Option<String>, max_length = 2000)]
    message: Option<Note>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct Committed {
    /// The id of the version the save ended.
    version: u64,
}

#[utoipa::path(
    post,
    path = "/api/rooms/{room_id}/history/commit",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
    ),
    request_body = CommitBody,
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The newest version is saved: it ends at the project's last change, with the name and note given", body = Committed),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller cannot write to the room", body = Detail),
        (status = 404, description = "No such room, or the caller is not a member", body = Detail),
        (status = 409, description = "Nothing changed since the last saved version", body = Detail),
        (status = 422, description = "The name is blank or over 100 characters, the note over 2000, or the body names a document: saves catalogues only", body = Detail),
    )
)]
async fn commit(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
    JsonBody(body): JsonBody<CommitBody>,
) -> Result<Json<Committed>, ApiError> {
    lifecycle::require_role(&state, &room_id, user.id, Role::Editor).await?;
    let name = body.name.as_ref().map(|name| name.0.as_str());
    let message = body.message.as_ref().and_then(|note| note.0.as_deref());
    let version = save_newest(&state, &room_id, user.id, name, message).await?;
    Ok(Json(Committed { version }))
}

/// Saves the room's newest version, ending at its last record, with the name and note
/// given; its id. 409 when nothing changed since the last saved version.
pub(crate) async fn save_newest(
    state: &AppState,
    room_id: &str,
    user: Uuid,
    name: Option<&str>,
    message: Option<&str>,
) -> Result<u64, ApiError> {
    save_newest_if_changed(state, room_id, Some(user), name, message)
        .await?
        .ok_or_else(nothing_changed)
}

/// Saves the room's newest version as `user`, or the server when `None`; its id, `None`
/// when nothing changed since the last saved version.
pub(crate) async fn save_newest_if_changed(
    state: &AppState,
    room_id: &str,
    user: Option<Uuid>,
    name: Option<&str>,
    message: Option<&str>,
) -> Result<Option<u64>, RoomError> {
    let saved = marks(&db::version_commits(&state.pool, room_id).await?);
    let after = saved.last().copied().and_then(row);
    let Some(records) = state
        .rooms
        .read_log(room_id, 1, move |_, slots| spans(slots, &saved).pop())
        .await?
        .and_then(rows)
    else {
        return Ok(None);
    };
    let Ok(version) = u64::try_from(*records.start()) else {
        return Ok(None);
    };
    let saving = db::SavedVersion {
        records,
        name,
        message,
        after,
    };
    Ok(db::save_version(&state.pool, room_id, &saving, user)
        .await?
        .then_some(version))
}

fn nothing_changed() -> ApiError {
    ApiError::Conflict("Nothing changed since the last saved version.".to_owned())
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct RestoreBody {
    /// Restore only this catalogue and its members' events; the whole room when absent.
    #[serde(default)]
    catalogue: Option<String>,
    /// Restore this workspace document instead of the catalogues.
    #[serde(default)]
    document: Option<Uuid>,
    /// With the manifest as `document`, restore only this path's entry.
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct Restored {
    /// False when the room already equalled the version.
    restored: bool,
}

#[utoipa::path(
    post,
    path = "/api/rooms/{room_id}/history/{version}/restore",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("version" = u64, Path, description = "Version id: its first record"),
    ),
    request_body = RestoreBody,
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The room, the catalogue, the document or the manifest path equals the version again, through one new change by the caller", body = Restored),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller cannot write to the room", body = Detail),
        (status = 404, description = "No such room, document, version or membership", body = Detail),
        (status = 409, description = "The room reached its storage limit", body = Detail),
        (status = 422, description = "A path without the manifest as document, or a catalogue and a document together", body = Detail),
    )
)]
async fn restore(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, version)): ApiPath<(String, u64)>,
    JsonBody(body): JsonBody<RestoreBody>,
) -> Result<Json<Restored>, ApiError> {
    lifecycle::require_role(&state, &room_id, user.id, Role::Editor).await?;
    if body.catalogue.is_some() && body.document.is_some() {
        return Err(unprocessable("Name a catalogue or a document, not both"));
    }
    let (key, kind, role) = history_key(&state, &room_id, body.document, user.id).await?;
    if !role.can_write() {
        return Err(ApiError::Forbidden(
            "This proposal is not yours, or its round no longer takes proposals".to_owned(),
        ));
    }
    if body.path.is_some() && kind != Some(DocumentKind::Manifest) {
        return Err(unprocessable("A path is restored from the manifest only"));
    }
    let saved = saved(&state, &key).await?;
    let update = state
        .rooms
        .read_log(&key, 2, move |records, slots| {
            let end = span(slots, &saved, version)?.end;
            let update = match body.document {
                Some(_) => document_update(records, end, body.path.as_deref()),
                None => restore_update(records, end, body.catalogue.as_deref()),
            };
            Some(update)
        })
        .await?
        .ok_or_else(no_such_version)?;
    let allowance = state.rooms.allowance(&state.pool, &key).await?;
    let restored = state
        .rooms
        .write_as(
            &key,
            allowance,
            Author::Restore {
                user: user.id,
                version,
            },
            update,
        )
        .await?;
    Ok(Json(Restored { restored }))
}

/// The update that makes the room as written so far equal to the version ending before
/// record `end` again. It is built on a replay, the way a client edits its replica: edits
/// the room takes meanwhile merge with it as any concurrent edit does.
fn restore_update(records: &[Bytes], end: usize, catalogue: Option<&str>) -> Vec<u8> {
    let (past, present) = before_and_after(records, end, records.len());
    let mut txn = present.transact_mut();
    model::restore(&mut txn, &past, catalogue);
    txn.encode_update_v1()
}

/// The same for a workspace document, or only one `path` of its manifest.
fn document_update(records: &[Bytes], end: usize, path: Option<&str>) -> Vec<u8> {
    let (past, present) = before_and_after(records, end, records.len());
    let mut txn = present.transact_mut();
    restore_document(&mut txn, &past, path);
    txn.encode_update_v1()
}

/// The version's first and last records as the database stores them, once the caller may
/// write and the version exists.
async fn writable_version(
    state: &AppState,
    room_id: &str,
    user: Uuid,
    version: u64,
) -> Result<RangeInclusive<i64>, ApiError> {
    lifecycle::require_role(state, room_id, user, Role::Editor).await?;
    let saved = saved(state, room_id).await?;
    state
        .rooms
        .read_log(room_id, 1, move |_, slots| span(slots, &saved, version))
        .await?
        .and_then(rows)
        .ok_or_else(no_such_version)
}

fn rows(span: Range<usize>) -> Option<RangeInclusive<i64>> {
    Some(row(span.start)?..=row(span.end.checked_sub(1)?)?)
}

pub(crate) async fn saved(state: &AppState, room_id: &str) -> Result<Saved, ApiError> {
    Ok(marks(&db::version_commits(&state.pool, room_id).await?))
}

fn marks(commits: &BTreeMap<i64, Option<String>>) -> Saved {
    commits
        .keys()
        .filter_map(|&record| u64::try_from(record).ok())
        .collect()
}

fn row(record: impl TryInto<i64>) -> Option<i64> {
    record.try_into().ok()
}

/// Which versions a catalogue's history shows: those that touched the catalogue or one of
/// its members' events; for a smart catalogue, any event. Their counts are of the members:
/// the events it lists, and those its rule yields now.
struct Filter {
    catalogue: String,
    smart: bool,
    members: BTreeSet<String>,
}

impl Filter {
    /// The rule and the members are read from the room now: an open room answers from
    /// memory, which is the browser's case.
    async fn new(state: &AppState, room_id: &str, catalogue: String) -> Result<Self, ApiError> {
        let uuid = catalogue.clone();
        let (smart, members) = state
            .rooms
            .read(room_id, move |doc: &Doc| {
                let txn = doc.transact();
                (has_rule(&txn, &uuid), members(&txn, &uuid))
            })
            .await?;
        Ok(Self {
            catalogue,
            smart,
            members,
        })
    }

    fn keeps(&self, version: &Version) -> bool {
        version.catalogues.contains(&self.catalogue)
            || (self.smart && !version.events.is_empty())
            || version.events.meets(&self.members)
    }
}

fn has_rule<T: ReadTxn>(txn: &T, catalogue: &str) -> bool {
    model::catalogue(txn, catalogue).is_some_and(|summary| summary.rule.is_some())
}

/// The room's versions, newest first, grouped again only when its log changed.
pub(crate) async fn room_versions(
    state: &AppState,
    room_id: &str,
) -> Result<Vec<Version>, ApiError> {
    let saved = saved(state, room_id).await?;
    let cache = state.rooms.version_cache().clone();
    let id = room_id.to_owned();
    Ok(state
        .rooms
        .read_log(room_id, 1, move |records, slots| {
            cache.versions(&id, records, slots, &saved)
        })
        .await?)
}

/// The versions as a member with `role` sees them: named, with their authors, who stay
/// anonymous to a public project's readers. `key` is the project's, or one document's.
pub(crate) async fn views(
    state: &AppState,
    key: &str,
    role: Role,
    versions: Vec<Version>,
) -> Result<Vec<VersionView>, ApiError> {
    let commits = db::version_commits(&state.pool, key).await?;
    let names = db::version_names(&state.pool, key).await?;
    let project = documents::project_of(key);
    let anonymous = db::is_public(&state.pool, project).await? && !role.can_write();
    let emails = if anonymous {
        author_labels(state, &versions).await?
    } else {
        author_emails(state, &versions).await?
    };
    let server = store::relayed(&state.pool, project)
        .await?
        .map(|relayed| relayed.source);
    Ok(versions
        .into_iter()
        .map(|version| view(version, &names, &commits, &emails, server.as_deref()))
        .map(|view| author_unless_anonymous(anonymous, view))
        .collect())
}

/// The emails of the versions' authors; an account deleted since has none.
async fn author_emails(
    state: &AppState,
    versions: &[Version],
) -> Result<HashMap<Uuid, String>, ApiError> {
    let mut emails = HashMap::new();
    for author in versions.iter().filter_map(|version| version.author) {
        if emails.contains_key(&author) {
            continue;
        }
        if let Some(account) = db::user_by_id(&state.pool, author).await? {
            emails.insert(author, account.email);
        }
    }
    Ok(emails)
}

/// What a public catalogue's reader sees of its authors: who looks after it, or who
/// contributed to it, never an email.
async fn author_labels(
    state: &AppState,
    versions: &[Version],
) -> Result<HashMap<Uuid, String>, ApiError> {
    let mut labels = HashMap::new();
    for author in versions.iter().filter_map(|version| version.author) {
        if labels.contains_key(&author) {
            continue;
        }
        let label = if db::is_curator(&state.pool, author).await? {
            "a curator"
        } else {
            "a contributor"
        };
        labels.insert(author, label.to_owned());
    }
    Ok(labels)
}

fn view(
    version: Version,
    names: &BTreeMap<i64, String>,
    commits: &BTreeMap<i64, Option<String>>,
    emails: &HashMap<Uuid, String>,
    server: Option<&str>,
) -> VersionView {
    let commit = row(version.last).and_then(|last| commits.get(&last));
    let stored = row(version.id)
        .zip(row(version.last))
        .and_then(|(first, last)| names.range(first..=last).next())
        .map(|(_, name)| name.clone());
    let author_email = match version.author {
        Some(author) => emails.get(&author).cloned(),
        None => server.map(str::to_owned),
    };
    VersionView {
        author_color: author_email.as_deref().map(presence::color),
        author_email,
        name: stored.or_else(|| version.auto_name.clone()),
        committed: commit.is_some(),
        message: commit.cloned().flatten(),
        version,
    }
}

/// A public catalogue's reader gets no account id either: it would tell authors apart.
fn author_unless_anonymous(anonymous: bool, mut view: VersionView) -> VersionView {
    if anonymous {
        view.version.author = None;
    }
    view
}

fn no_such_version() -> ApiError {
    ApiError::NotFound("No such version".to_owned())
}

#[cfg(test)]
mod tests {
    use super::copy_name;

    #[test]
    fn a_copy_name_carries_the_date_when_the_version_has_one() {
        assert_eq!(
            copy_name("Paper", Some(1_700_000_000_000)),
            "Copied from Paper, 2023-11-14 22:13 UTC"
        );
        assert_eq!(copy_name("Paper", None), "Copied from Paper");
    }

    #[test]
    fn a_copy_name_fits_a_version_name() {
        assert_eq!(copy_name(&"x".repeat(200), None).chars().count(), 100);
    }
}
