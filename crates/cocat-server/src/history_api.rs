//! A room's version history under `/api`: the versions, what each changed, the room as it
//! was, and the names people give versions.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::{Range, RangeInclusive};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use bytes::Bytes;
use cocat_core::model;
use cocat_core::role::Role;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::macros::format_description;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;
use yrs::{Doc, ReadTxn, StateVector, Transact};

use crate::api::{self, ApiError, ApiPath, CurrentUser, JsonBody, membership};
use crate::db::RoomRow;
use crate::history::diff::{Diff, before_and_after, diff, members, replay_until};
use crate::history::versions::{Saved, Version, span, spans, versions};
use crate::openapi::Detail;
use crate::room::Author;
use crate::{AppState, db, lifecycle, presence};

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
struct CatalogueQuery {
    catalogue: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct HistoryQuery {
    catalogue: Option<String>,
    event: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct VersionView {
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
    ),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The versions, newest first", body = History),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such room, or the caller is not a member", body = Detail),
    )
)]
async fn list(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<History>, ApiError> {
    membership(&state, &room_id, user.id).await?;
    let filter = match query.catalogue {
        Some(uuid) => Some(Filter::new(&state, &room_id, uuid).await?),
        None => None,
    };
    let commits = db::version_commits(&state.pool, &room_id).await?;
    let saved = marks(&commits);
    // simplify: every request replays the whole log (0.7 s on the largest production
    // room); cache the versions per room keyed by its record count when that hurts.
    let mut versions = state
        .rooms
        .read_log(&room_id, 1, move |records, slots| {
            versions(records, slots, &saved)
        })
        .await?;
    if let Some(filter) = filter {
        versions.retain(|version| filter.keeps(version));
        for version in &mut versions {
            version.count_only(&filter.members);
        }
    }
    if let Some(event) = query.event {
        versions.retain(|version| version.events.contains(&event));
    }
    let names = db::version_names(&state.pool, &room_id).await?;
    let emails = author_emails(&state, &versions).await?;
    let versions = versions
        .into_iter()
        .map(|version| view(version, &names, &commits, &emails))
        .collect();
    Ok(Json(History { versions }))
}

#[utoipa::path(
    get,
    path = "/api/rooms/{room_id}/history/{version}",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("version" = u64, Path, description = "Version id: its first record"),
        ("catalogue" = Option<String>, Query, description = "Only this catalogue and its members"),
    ),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "What the version changed", body = Diff),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such room, version or membership", body = Detail),
    )
)]
async fn changes(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, version)): ApiPath<(String, u64)>,
    Query(query): Query<CatalogueQuery>,
) -> Result<Json<Diff>, ApiError> {
    membership(&state, &room_id, user.id).await?;
    let saved = saved(&state, &room_id).await?;
    let changes = state
        .rooms
        .read_log(&room_id, 2, move |records, slots| {
            let span = span(slots, &saved, version)?;
            let (before, after) = before_and_after(records, span.start, span.end);
            Some(diff(&before, &after, query.catalogue.as_deref()))
        })
        .await?
        .ok_or_else(no_such_version)?;
    Ok(Json(changes))
}

#[utoipa::path(
    get,
    path = "/api/rooms/{room_id}/history/{version}/state",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("version" = u64, Path, description = "Version id: its first record"),
    ),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The room as of the version, as one Yjs v1 update", content_type = "application/octet-stream", body = Vec<u8>),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such room, version or membership", body = Detail),
    )
)]
async fn state_at(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, version)): ApiPath<(String, u64)>,
) -> Result<impl IntoResponse, ApiError> {
    membership(&state, &room_id, user.id).await?;
    let saved = saved(&state, &room_id).await?;
    let update = state
        .rooms
        .read_log(&room_id, 1, move |records, slots| {
            Some(state_until(records, span(slots, &saved, version)?.end))
        })
        .await?
        .ok_or_else(no_such_version)?;
    Ok(([(header::CONTENT_TYPE, "application/octet-stream")], update))
}

/// The room as of the version ending before record `end`, as one update.
fn state_until(records: &[Bytes], end: usize) -> Vec<u8> {
    replay_until(&records[..end], u64::MAX)
        .transact()
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
    let (update, started_ms) = state
        .rooms
        .read_log(&room_id, 1, move |records, slots| {
            let span = span(slots, &saved, version)?;
            let started_ms = slots
                .get(span.start)
                .copied()
                .flatten()
                .map(|stamp| stamp.at_ms);
            Some((state_until(records, span.end), started_ms))
        })
        .await?
        .ok_or_else(no_such_version)?;
    let original = db::room_name(&state.pool, &room_id)
        .await?
        .unwrap_or_default();
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
async fn make_copy(
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
fn copy_name(project: &str, started_ms: Option<i64>) -> String {
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
        (status = 422, description = "The name is blank or longer than 100 characters", body = Detail),
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
        (status = 422, description = "The name is blank or over 100 characters, or the note over 2000", body = Detail),
    )
)]
async fn commit(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
    JsonBody(body): JsonBody<CommitBody>,
) -> Result<Json<Committed>, ApiError> {
    lifecycle::require_role(&state, &room_id, user.id, Role::Editor).await?;
    let saved = saved(&state, &room_id).await?;
    let after = saved.last().copied().and_then(row);
    let records = state
        .rooms
        .read_log(&room_id, 1, move |_, slots| spans(slots, &saved).pop())
        .await?
        .and_then(rows)
        .ok_or_else(nothing_changed)?;
    let version = u64::try_from(*records.start()).map_err(|_| nothing_changed())?;
    let saving = db::SavedVersion {
        records,
        name: body.name.as_ref().map(|name| name.0.as_str()),
        message: body.message.as_ref().and_then(|note| note.0.as_deref()),
        after,
    };
    if !db::save_version(&state.pool, &room_id, &saving, user.id).await? {
        return Err(nothing_changed());
    }
    Ok(Json(Committed { version }))
}

fn nothing_changed() -> ApiError {
    ApiError::Conflict("Nothing changed since the last saved version.".to_owned())
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct RestoreBody {
    /// Restore only this catalogue and its members' events; the whole room when absent.
    #[serde(default)]
    catalogue: Option<String>,
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
        (status = 200, description = "The room, or the catalogue, equals the version again, through one new change by the caller", body = Restored),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller cannot write to the room", body = Detail),
        (status = 404, description = "No such room, version or membership", body = Detail),
        (status = 409, description = "The room reached its storage limit", body = Detail),
    )
)]
async fn restore(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, version)): ApiPath<(String, u64)>,
    JsonBody(body): JsonBody<RestoreBody>,
) -> Result<Json<Restored>, ApiError> {
    lifecycle::require_role(&state, &room_id, user.id, Role::Editor).await?;
    let saved = saved(&state, &room_id).await?;
    let update = state
        .rooms
        .read_log(&room_id, 2, move |records, slots| {
            let end = span(slots, &saved, version)?.end;
            Some(restore_update(records, end, body.catalogue.as_deref()))
        })
        .await?
        .ok_or_else(no_such_version)?;
    let allowance = state.rooms.allowance(&state.pool, &room_id).await?;
    let restored = state
        .rooms
        .write_as(
            &room_id,
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

async fn saved(state: &AppState, room_id: &str) -> Result<Saved, ApiError> {
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

fn view(
    version: Version,
    names: &BTreeMap<i64, String>,
    commits: &BTreeMap<i64, Option<String>>,
    emails: &HashMap<Uuid, String>,
) -> VersionView {
    let commit = row(version.last).and_then(|last| commits.get(&last));
    let stored = row(version.id)
        .zip(row(version.last))
        .and_then(|(first, last)| names.range(first..=last).next())
        .map(|(_, name)| name.clone());
    let author_email = version
        .author
        .and_then(|author| emails.get(&author).cloned());
    VersionView {
        author_color: author_email.as_deref().map(presence::color),
        author_email,
        name: stored.or_else(|| version.auto_name.clone()),
        committed: commit.is_some(),
        message: commit.cloned().flatten(),
        version,
    }
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
