//! The JSON API under `/api`, plus the two legacy room routes the old UI still calls.
//! Every error is `{"detail": "<message>"}`, as FastAPI sends it.

use axum::Json;
use axum::Router;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Serialize;
use uuid::Uuid;

use crate::db::{self, DbError};
use crate::lifecycle::LifecycleError;
use crate::{AppState, auth};

pub fn router() -> Router<AppState> {
    Router::new().route("/me", get(me))
}

#[derive(Debug, Serialize)]
struct UserRead {
    id: Uuid,
    email: String,
    is_active: bool,
    is_superuser: bool,
    is_verified: bool,
}

impl From<db::User> for UserRead {
    fn from(user: db::User) -> Self {
        Self {
            id: user.id,
            email: user.email,
            is_active: user.is_active,
            is_superuser: user.is_superuser,
            is_verified: user.is_verified,
        }
    }
}

/// The caller, or a 401: the cookie must name an active user.
#[derive(Debug)]
pub struct CurrentUser(pub db::User);

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let id = auth::authenticate(&state.pool, &parts.headers, &state.secret)
            .await
            .map_err(|_| ApiError::Unauthorized)?;
        let user = db::user_by_id(&state.pool, id)
            .await?
            .ok_or(ApiError::Unauthorized)?;
        Ok(Self(user))
    }
}

#[derive(Debug)]
pub enum ApiError {
    Unauthorized,
    NotFound(String),
    Forbidden(String),
    Conflict(String),
    BadRequest(String),
    Internal,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, detail) = match self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "Unauthorized".to_owned()),
            Self::NotFound(detail) => (StatusCode::NOT_FOUND, detail),
            Self::Forbidden(detail) => (StatusCode::FORBIDDEN, detail),
            Self::Conflict(detail) => (StatusCode::CONFLICT, detail),
            Self::BadRequest(detail) => (StatusCode::BAD_REQUEST, detail),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error".to_owned(),
            ),
        };
        (status, Json(serde_json::json!({ "detail": detail }))).into_response()
    }
}

impl From<DbError> for ApiError {
    fn from(error: DbError) -> Self {
        tracing::error!(%error, "the database failed an API request");
        Self::Internal
    }
}

impl From<LifecycleError> for ApiError {
    fn from(error: LifecycleError) -> Self {
        match error {
            LifecycleError::NotFound => Self::NotFound("Not found".to_owned()),
            LifecycleError::Forbidden => Self::Forbidden("Forbidden".to_owned()),
            LifecycleError::Conflict(detail) => Self::Conflict(detail.to_owned()),
            LifecycleError::Db(error) => Self::from(error),
        }
    }
}

async fn me(CurrentUser(user): CurrentUser) -> Json<UserRead> {
    Json(user.into())
}

#[derive(Debug, Serialize)]
pub struct UserRooms {
    rooms: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RoomUsers {
    users: Vec<String>,
}

/// `GET /rooms`: the ids of the caller's rooms, for the old UI.
pub async fn legacy_rooms(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<UserRooms>, ApiError> {
    let rooms = db::list_user_rooms(&state.pool, user.id).await?;
    Ok(Json(UserRooms {
        rooms: rooms.into_iter().map(|(room, _)| room.id).collect(),
    }))
}

/// `GET /room/{room_id}/users`: the members' emails; a non-member sees 404, not 403.
pub async fn legacy_room_users(
    Path(room_id): Path<String>,
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<RoomUsers>, ApiError> {
    if db::role_in(&state.pool, &room_id, user.id).await?.is_none() {
        return Err(ApiError::NotFound(format!("Room {room_id} not found")));
    }
    let members = db::list_members(&state.pool, &room_id).await?;
    Ok(Json(RoomUsers {
        users: members
            .into_iter()
            .map(|(member, _)| member.email)
            .collect(),
    }))
}
