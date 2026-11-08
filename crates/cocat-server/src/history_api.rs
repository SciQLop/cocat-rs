//! A room's version history under `/api`: the versions, what each changed, the room as it
//! was, and the names people give versions.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use cocat_core::model;
use cocat_core::role::Role;
use serde::{Deserialize, Serialize};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;
use yrs::{Doc, ReadTxn, StateVector, Transact};

use crate::api::{ApiError, ApiPath, CurrentUser, JsonBody, membership};
use crate::history::diff::{Diff, diff, listed, replay_until};
use crate::history::versions::{Version, span, versions};
use crate::openapi::Detail;
use crate::{AppState, db, lifecycle};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list))
        .routes(routes!(changes))
        .routes(routes!(state_at))
        .routes(routes!(name, unname))
}

#[derive(Debug, Default, Deserialize)]
struct CatalogueQuery {
    catalogue: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct VersionView {
    #[serde(flatten)]
    version: Version,
    author_email: Option<String>,
    /// The name someone gave the version, else the one its import or materialization gave it.
    name: Option<String>,
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
        ("room_id" = String, Path, description = "Room id"),
        ("catalogue" = Option<String>, Query, description = "Only the versions that touched this catalogue or its members' events; for a smart catalogue, any event"),
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
    Query(query): Query<CatalogueQuery>,
) -> Result<Json<History>, ApiError> {
    membership(&state, &room_id, user.id).await?;
    let filter = match query.catalogue {
        Some(uuid) => Some(Filter::new(&state, &room_id, uuid).await?),
        None => None,
    };
    // simplify: every request replays the whole log (0.7 s on the largest production
    // room); cache the versions per room keyed by its record count when that hurts.
    let mut versions = state.rooms.read_log(&room_id, versions).await?;
    if let Some(filter) = filter {
        versions.retain(|version| filter.keeps(version));
    }
    let names = db::version_names(&state.pool, &room_id).await?;
    let emails = author_emails(&state, &versions).await?;
    let versions = versions
        .into_iter()
        .map(|version| view(version, &names, &emails))
        .collect();
    Ok(Json(History { versions }))
}

#[utoipa::path(
    get,
    path = "/api/rooms/{room_id}/history/{version}",
    params(
        ("room_id" = String, Path, description = "Room id"),
        ("version" = u64, Path, description = "Version id: its last record"),
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
    let changes = state
        .rooms
        .read_log(&room_id, move |records, slots| {
            let span = span(slots, version)?;
            let before = replay_until(&records[..span.start], u64::MAX);
            let after = replay_until(&records[..span.end], u64::MAX);
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
        ("room_id" = String, Path, description = "Room id"),
        ("version" = u64, Path, description = "Version id: its last record"),
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
    let update = state
        .rooms
        .read_log(&room_id, move |records, slots| {
            span(slots, version)?;
            let past = replay_until(records, version);
            let update = past
                .transact()
                .encode_state_as_update_v1(&StateVector::default());
            Some(update)
        })
        .await?
        .ok_or_else(no_such_version)?;
    Ok(([(header::CONTENT_TYPE, "application/octet-stream")], update))
}

#[utoipa::path(
    put,
    path = "/api/rooms/{room_id}/history/{version}/name",
    params(
        ("room_id" = String, Path, description = "Room id"),
        ("version" = u64, Path, description = "Version id: its last record"),
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
    let version = writable_version(&state, &room_id, user.id, version).await?;
    db::name_version(&state.pool, &room_id, version, &body.name.0, user.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/api/rooms/{room_id}/history/{version}/name",
    params(
        ("room_id" = String, Path, description = "Room id"),
        ("version" = u64, Path, description = "Version id: its last record"),
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
    let version = writable_version(&state, &room_id, user.id, version).await?;
    db::unname_version(&state.pool, &room_id, version).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The version as the database stores it, once the caller may write and the version exists.
async fn writable_version(
    state: &AppState,
    room_id: &str,
    user: Uuid,
    version: u64,
) -> Result<i64, ApiError> {
    lifecycle::require_role(state, room_id, user, Role::Editor).await?;
    let exists = state
        .rooms
        .read_log(room_id, move |_, slots| span(slots, version).is_some())
        .await?;
    match i64::try_from(version) {
        Ok(version) if exists => Ok(version),
        _ => Err(no_such_version()),
    }
}

/// Which versions a catalogue's history shows: those that touched the catalogue or one of
/// its members' events; for a smart catalogue, any event.
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
                (has_rule(&txn, &uuid), listed(&txn, &uuid))
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
            || !version.events.is_disjoint(&self.members)
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
    emails: &HashMap<Uuid, String>,
) -> VersionView {
    let stored = i64::try_from(version.id)
        .ok()
        .and_then(|id| names.get(&id))
        .cloned();
    VersionView {
        author_email: version
            .author
            .and_then(|author| emails.get(&author).cloned()),
        name: stored.or_else(|| version.auto_name.clone()),
        version,
    }
}

fn no_such_version() -> ApiError {
    ApiError::NotFound("No such version".to_owned())
}
