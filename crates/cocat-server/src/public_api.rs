//! The public catalogues under `/api/public`: the index every signed-in account reads, and
//! a new one, which only curators make. Each is a project, opened like any other.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use cocat_core::role::Role;
use cocat_core::room_id::public_id;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::AppState;
use crate::api::{ApiError, CurrentUser, JsonBody};
use crate::db::{self, PublicRoom};
use crate::lifecycle::{self, Noun};
use crate::openapi::Detail;

const NOT_A_CURATOR: &str = "Only curators can create public catalogues";
const BAD_KEY: &str =
    "A key is dot-separated parts of letters, digits, '_' and '-', at most 64 characters";
const LONG_FOLDER: &str = "Enter a folder or a source of at most 200 characters";
const LONG_DESCRIPTION: &str = "Enter a description of at most 2000 characters";
const MAX_LABEL: usize = 200;
const MAX_DESCRIPTION: usize = 2000;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(index, create))
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
    /// `editor` for curators, `viewer` for everyone else.
    role: &'static str,
    /// When its events last changed (RFC 3339), `null` while it holds none.
    changed: Option<String>,
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
    let listed = db::public_rooms(&state.pool)
        .await?
        .into_iter()
        .map(|room| out(&state, room, role))
        .collect();
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
        (status = 409, description = "A public catalogue has that key", body = Detail),
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
    let room = checked(body)?;
    if db::room_exists(&state.pool, &room.id).await? {
        return Err(ApiError::Conflict(format!(
            "A public catalogue already has the id {}",
            room.id
        )));
    }
    db::create_public_room(&state.pool, &room, user.id).await?;
    Ok((StatusCode::CREATED, Json(out(&state, room, Role::Editor))))
}

async fn role_of(state: &AppState, user: Uuid) -> Result<Role, ApiError> {
    Ok(match db::is_curator(&state.pool, user).await? {
        true => Role::Editor,
        false => Role::Viewer,
    })
}

fn out(state: &AppState, room: PublicRoom, role: Role) -> PublicOut {
    let changed = state
        .rooms
        .room_changed(&room.id)
        .and_then(|at| OffsetDateTime::from(at).format(&Rfc3339).ok());
    PublicOut {
        id: room.id,
        name: room.name,
        folder: room.folder,
        source: room.source,
        description: room.description,
        role: role.as_str(),
        changed,
    }
}

fn checked(body: NewPublic) -> Result<PublicRoom, ApiError> {
    let invalid = |detail: &str| ApiError::BadRequest(detail.to_owned());
    let id = match body.key.as_deref() {
        Some(key) => public_id(key).map_err(|_| invalid(BAD_KEY))?,
        None => public_id(&Uuid::new_v4().simple().to_string()).map_err(|_| invalid(BAD_KEY))?,
    };
    let name = lifecycle::clean_name(&body.name, Noun::Room)?;
    Ok(PublicRoom {
        id,
        name,
        folder: label(body.folder, MAX_LABEL).map_err(|_| invalid(LONG_FOLDER))?,
        source: label(body.source, MAX_LABEL).map_err(|_| invalid(LONG_FOLDER))?,
        description: label(body.description, MAX_DESCRIPTION)
            .map_err(|_| invalid(LONG_DESCRIPTION))?,
    })
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
