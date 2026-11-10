//! The public catalogues under `/api/public`: the index every signed-in account reads, and
//! the new ones and changes to what the index says, which only curators make. Each is a
//! project, opened like any other.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use cocat_core::model;
use cocat_core::role::Role;
use cocat_core::room_id::public_id;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;
use yrs::Transact;

use crate::AppState;
use crate::api::{ApiError, ApiPath, CurrentUser, JsonBody};
use crate::db::{self, PublicRoom};
use crate::lifecycle::{self, Noun};
use crate::openapi::Detail;
use crate::relay::store::{self, RelayedRow};

const NOT_A_CURATOR: &str = "Only curators can create public catalogues";
const NOT_A_CURATOR_TO_CHANGE: &str = "Only curators can change public catalogues";
const NO_SUCH_CATALOGUE: &str = "No such public catalogue";
const BAD_KEY: &str =
    "A key is dot-separated parts of letters, digits, '_' and '-', at most 64 characters";
const LONG_FOLDER: &str = "Enter a folder or a source of at most 200 characters";
const LONG_DESCRIPTION: &str = "Enter a description of at most 2000 characters";
const MAX_LABEL: usize = 200;
const MAX_DESCRIPTION: usize = 2000;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(index, create))
        .routes(routes!(change))
}

/// A public catalogue in the index.
#[derive(Debug, Serialize, utoipa::ToSchema)]
struct PublicOut {
    id: String,
    name: String,
    /// Where it sits in the index, `/`-separated: "AMDA/Solar wind".
    folder: Option<String>,
    /// Where its events come from: "AMDA".
    source: Option<String>,
    description: Option<String>,
    /// `editor` for curators, `viewer` for everyone else and for a relayed catalogue.
    role: &'static str,
    /// When its events last changed (RFC 3339), `null` while it holds none.
    changed: Option<String>,
    /// How many events it holds outside the trash; `null` when it could not be counted.
    count: Option<i64>,
    /// The provider it follows; `null` for a catalogue people keep.
    relay: Option<RelayOut>,
}

/// Where a relayed catalogue comes from.
#[derive(Debug, Serialize, utoipa::ToSchema)]
struct RelayOut {
    /// The provider's name: "DONKI (NASA CCMC)".
    source: String,
    /// The list at its source.
    url: Option<String>,
    /// When the relay last refreshed it (RFC 3339).
    refreshed_at: Option<String>,
    /// False once the provider stopped publishing it: it stays as it was last refreshed.
    published: bool,
}

impl From<RelayedRow> for RelayOut {
    fn from(row: RelayedRow) -> Self {
        Self {
            source: row.source,
            url: row.url,
            refreshed_at: row.refreshed_at,
            published: row.published,
        }
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct NewPublic {
    name: String,
    folder: Option<String>,
    source: Option<String>,
    description: Option<String>,
    /// Makes the id `public.<key>`, so an import finds its catalogue again; a random one
    /// without it.
    key: Option<String>,
}

/// What to change; a field left out stays, a blank one is cleared (the name cannot be).
#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct PublicChanges {
    name: Option<String>,
    folder: Option<String>,
    source: Option<String>,
    description: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/public",
    security(("cookie" = [])),
    responses(
        (status = 200, description = "Every public catalogue", body = [PublicOut]),
        (status = 401, description = "No valid session", body = Detail),
    )
)]
async fn index(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Vec<PublicOut>>, ApiError> {
    let role = role_of(&state, user.id).await?;
    let mut listed = Vec::new();
    for (room, relay) in store::public_index(&state.pool).await? {
        listed.push(out(&state, room, role, relay).await);
    }
    Ok(Json(listed))
}

#[utoipa::path(
    post,
    path = "/api/public",
    request_body = NewPublic,
    security(("cookie" = [])),
    responses(
        (status = 201, description = "The new public catalogue", body = PublicOut),
        (status = 400, description = "A field is empty, too long or not a key", body = Detail),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller is not a curator", body = Detail),
        (status = 409, description = "A public catalogue has that key, or a provider's catalogues use it", body = Detail),
        (status = 422, description = "The body is not a public catalogue", body = Detail),
    )
)]
async fn create(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    JsonBody(body): JsonBody<NewPublic>,
) -> Result<(StatusCode, Json<PublicOut>), ApiError> {
    if !db::is_curator(&state.pool, user.id).await? {
        return Err(ApiError::Forbidden(NOT_A_CURATOR.to_owned()));
    }
    refuse_provider_ids(&state, body.key.as_deref())?;
    let room = checked(body)?;
    if !db::create_public_room(&state.pool, &room, Some(user.id)).await? {
        return Err(ApiError::Conflict(format!(
            "A public catalogue already has the id {}",
            room.id
        )));
    }
    Ok((
        StatusCode::CREATED,
        Json(out(&state, room, Role::Editor, None).await),
    ))
}

#[utoipa::path(
    patch,
    path = "/api/public/{room_id}",
    params(("room_id" = String, Path, description = "The public catalogue's id")),
    request_body = PublicChanges,
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The public catalogue as changed", body = PublicOut),
        (status = 400, description = "A field is empty or too long", body = Detail),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller is not a curator", body = Detail),
        (status = 404, description = "No public catalogue has that id", body = Detail),
        (status = 409, description = "It follows a provider: nobody changes it by hand", body = Detail),
        (status = 422, description = "The body is not a change", body = Detail),
    )
)]
async fn change(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
    JsonBody(body): JsonBody<PublicChanges>,
) -> Result<Json<PublicOut>, ApiError> {
    if !db::is_curator(&state.pool, user.id).await? {
        return Err(ApiError::Forbidden(NOT_A_CURATOR_TO_CHANGE.to_owned()));
    }
    let room = db::public_room(&state.pool, &room_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(NO_SUCH_CATALOGUE.to_owned()))?;
    lifecycle::refuse_relayed(&state, &room_id).await?;
    let room = changed(room, body)?;
    db::update_public_room(&state.pool, &room).await?;
    Ok(Json(out(&state, room, Role::Editor, None).await))
}

/// `public.<provider>.…` is where the relay puts a provider's catalogues.
fn refuse_provider_ids(state: &AppState, key: Option<&str>) -> Result<(), ApiError> {
    let Some(key) = key else {
        return Ok(());
    };
    match state
        .providers
        .iter()
        .find(|provider| key.starts_with(&format!("{}.", provider.id())))
    {
        Some(provider) => Err(ApiError::Conflict(format!(
            "Ids starting with public.{}. belong to the {} catalogues",
            provider.id(),
            provider.source()
        ))),
        None => Ok(()),
    }
}

async fn role_of(state: &AppState, user: Uuid) -> Result<Role, ApiError> {
    Ok(match db::is_curator(&state.pool, user).await? {
        true => Role::Editor,
        false => Role::Viewer,
    })
}

/// The row as `role` sees it; a relayed catalogue is read-only for everyone.
async fn out(
    state: &AppState,
    room: PublicRoom,
    role: Role,
    relay: Option<RelayedRow>,
) -> PublicOut {
    let role = match relay {
        Some(_) => Role::Viewer,
        None => role,
    };
    let changed = state
        .rooms
        .room_changed(&room.id)
        .and_then(|at| OffsetDateTime::from(at).format(&Rfc3339).ok());
    let count = counted(state, &room).await;
    PublicOut {
        id: room.id,
        name: room.name,
        folder: room.folder,
        source: room.source,
        description: room.description,
        role: role.as_str(),
        changed,
        count,
        relay: relay.map(RelayOut::from),
    }
}

/// The events the catalogue holds: the stored count while the room file is the one it was
/// taken from, else a fresh count, stored for the next look. A failed count answers the
/// stored one.
// simplify: the index counts each changed catalogue while it answers, so the first look after
// a bulk import loads every room the import wrote, once. Count when a room unloads instead if
// that wait shows.
async fn counted(state: &AppState, room: &PublicRoom) -> Option<i64> {
    let file = file_stamp(state, &room.id);
    if room.counted_file.as_deref() == Some(file.as_str()) {
        return room.public_count;
    }
    let read = state
        .rooms
        .read(&room.id, |doc| model::live_event_count(&doc.transact()))
        .await;
    let count = match read.map(i64::try_from) {
        Ok(Ok(count)) => count,
        Ok(Err(_)) => return room.public_count,
        Err(error) => {
            tracing::warn!(room = %room.id, %error, "the public catalogue could not be counted");
            return room.public_count;
        }
    };
    if let Err(error) = db::store_public_count(&state.pool, &room.id, count, &file).await {
        tracing::warn!(room = %room.id, %error, "the public catalogue count was not stored");
    }
    Some(count)
}

/// Names the room file as it is now. The file only grows between compactions, so its size
/// tells two writes apart even when the clock gives them the same modification time.
fn file_stamp(state: &AppState, room_id: &str) -> String {
    let modified = state
        .rooms
        .room_changed(room_id)
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos());
    format!("{}:{modified}", state.rooms.room_size(room_id))
}

fn checked(body: NewPublic) -> Result<PublicRoom, ApiError> {
    let invalid = |detail: &str| ApiError::BadRequest(detail.to_owned());
    let id = match body.key.as_deref() {
        Some(key) => public_id(key).map_err(|_| invalid(BAD_KEY))?,
        None => public_id(&Uuid::new_v4().simple().to_string()).map_err(|_| invalid(BAD_KEY))?,
    };
    Ok(PublicRoom {
        id,
        name: lifecycle::clean_name(&body.name, Noun::Room)?,
        folder: folder_or_source(body.folder)?,
        source: folder_or_source(body.source)?,
        description: description(body.description)?,
        public_count: None,
        counted_file: None,
    })
}

fn changed(room: PublicRoom, body: PublicChanges) -> Result<PublicRoom, ApiError> {
    Ok(PublicRoom {
        name: match body.name {
            None => room.name,
            Some(name) => lifecycle::clean_name(&name, Noun::Room)?,
        },
        folder: match body.folder {
            None => room.folder,
            given => folder_or_source(given)?,
        },
        source: match body.source {
            None => room.source,
            given => folder_or_source(given)?,
        },
        description: match body.description {
            None => room.description,
            given => description(given)?,
        },
        ..room
    })
}

pub(crate) fn folder_or_source(value: Option<String>) -> Result<Option<String>, ApiError> {
    label(value, MAX_LABEL).map_err(|_| ApiError::BadRequest(LONG_FOLDER.to_owned()))
}

pub(crate) fn description(value: Option<String>) -> Result<Option<String>, ApiError> {
    label(value, MAX_DESCRIPTION).map_err(|_| ApiError::BadRequest(LONG_DESCRIPTION.to_owned()))
}

/// A trimmed optional field: blank is none, longer than `max` characters is refused.
fn label(value: Option<String>, max: usize) -> Result<Option<String>, ()> {
    let value = value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    match value {
        Some(value) if value.chars().count() > max => Err(()),
        value => Ok(value),
    }
}
