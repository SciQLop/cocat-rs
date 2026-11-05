//! The JSON API under `/api`, plus the two legacy room routes the old UI still calls.
//! Every error is `{"detail": "<message>"}`, as FastAPI sends it.

use axum::Json;
use axum::Router;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{FromRequest, FromRequestParts, Path, Request, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use cocat_core::role::Role;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::accounts::AccountError;
use crate::db::{self, DbError, RoomRow};
use crate::lifecycle::{self, LifecycleError};
use crate::{AppState, auth};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me", get(me))
        .route("/rooms", get(list_rooms).post(create_room))
        .route("/rooms/{room_id}", patch(rename_room).delete(delete_room))
        .route("/rooms/{room_id}/members", get(list_members))
        .route(
            "/rooms/{room_id}/members/{user_id}",
            patch(set_member_role).delete(remove_member),
        )
        .route("/rooms/{room_id}/owner", post(transfer_ownership))
        .merge(crate::invitations_api::router())
}

#[derive(Debug, Serialize)]
pub(crate) struct UserRead {
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
    Unprocessable(String),
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
            Self::Unprocessable(detail) => (StatusCode::UNPROCESSABLE_ENTITY, detail),
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

impl From<AccountError> for ApiError {
    fn from(error: AccountError) -> Self {
        match error {
            AccountError::Duplicate => Self::BadRequest("REGISTER_USER_ALREADY_EXISTS".to_owned()),
            AccountError::Db(error) => Self::from(error),
            AccountError::BadVerifyToken => Self::BadRequest("VERIFY_USER_BAD_TOKEN".to_owned()),
            AccountError::AlreadyVerified => {
                Self::BadRequest("VERIFY_USER_ALREADY_VERIFIED".to_owned())
            }
            AccountError::BadResetToken => Self::BadRequest("RESET_PASSWORD_BAD_TOKEN".to_owned()),
            AccountError::EmailInUse => {
                Self::BadRequest("UPDATE_USER_EMAIL_ALREADY_EXISTS".to_owned())
            }
            AccountError::Clock => {
                tracing::error!("the clock is before the unix epoch");
                Self::Internal
            }
            AccountError::Sign(error) => {
                tracing::error!(%error, "cannot sign a link");
                Self::Internal
            }
        }
    }
}

impl From<LifecycleError> for ApiError {
    fn from(error: LifecycleError) -> Self {
        match error {
            LifecycleError::NotFound(detail) => Self::NotFound(detail),
            LifecycleError::Forbidden(detail) => Self::Forbidden(detail),
            LifecycleError::Conflict(detail) => Self::Conflict(detail.to_owned()),
            LifecycleError::Invalid(detail) => Self::BadRequest(detail.to_owned()),
            LifecycleError::Db(error) => Self::from(error),
            LifecycleError::Role(error) => {
                tracing::error!(%error, "a role column holds an unknown role");
                Self::Internal
            }
            LifecycleError::Random(error) => {
                tracing::error!(%error, "the random source failed");
                Self::Internal
            }
        }
    }
}

/// A JSON body whose rejection is an API error. FastAPI answers a syntax or schema error
/// with 422 and `{"detail": ...}`; axum's own rejection is 400 and plain text.
#[derive(Debug)]
pub(crate) struct JsonBody<T>(pub(crate) T);

impl<T> FromRequest<AppState> for JsonBody<T>
where
    Json<T>: FromRequest<AppState, Rejection = JsonRejection>,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &AppState) -> Result<Self, Self::Rejection> {
        let Json(value) = Json::<T>::from_request(request, state)
            .await
            .map_err(|rejection| ApiError::Unprocessable(rejection.body_text()))?;
        Ok(Self(value))
    }
}

/// A path parameter whose rejection is an API error, for the same reason as `JsonBody`.
#[derive(Debug)]
pub(crate) struct ApiPath<T>(pub(crate) T);

impl<T> FromRequestParts<AppState> for ApiPath<T>
where
    T: DeserializeOwned + Send,
{
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Path(value) = Path::<T>::from_request_parts(parts, state)
            .await
            .map_err(|rejection: PathRejection| ApiError::Unprocessable(rejection.body_text()))?;
        Ok(Self(value))
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct RoomOut {
    id: String,
    name: String,
    role: &'static str,
}

impl RoomOut {
    pub(crate) fn new(room: RoomRow, role: Role) -> Self {
        Self {
            id: room.id,
            name: room.name,
            role: role.as_str(),
        }
    }
}

#[derive(Debug, Serialize)]
struct MemberOut {
    user_id: Uuid,
    email: String,
    role: &'static str,
}

/// FastAPI's `Field(min_length=1, max_length=200)` counts characters, and a name outside
/// that range is a schema error: validating in the deserializer makes it the same 422.
#[derive(Debug, Deserialize)]
#[serde(try_from = "String")]
struct RoomName(String);

impl TryFrom<String> for RoomName {
    type Error = &'static str;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        match name.chars().count() {
            1..=200 => Ok(Self(name)),
            _ => Err("the room name must be 1 to 200 characters"),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RoomNameBody {
    name: RoomName,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AssignableRole {
    Viewer,
    Editor,
}

impl From<AssignableRole> for Role {
    fn from(role: AssignableRole) -> Self {
        match role {
            AssignableRole::Viewer => Role::Viewer,
            AssignableRole::Editor => Role::Editor,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RoleBody {
    role: AssignableRole,
}

#[derive(Debug, Deserialize)]
struct OwnerBody {
    user_id: Uuid,
}

async fn me(CurrentUser(user): CurrentUser) -> Json<UserRead> {
    Json(user.into())
}

async fn list_rooms(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Vec<RoomOut>>, ApiError> {
    let rooms = db::list_user_rooms(&state.pool, user.id)
        .await?
        .into_iter()
        .map(|(room, role)| RoomOut::new(room, role))
        .collect();
    Ok(Json(rooms))
}

async fn create_room(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    JsonBody(body): JsonBody<RoomNameBody>,
) -> Result<(StatusCode, Json<RoomOut>), ApiError> {
    let room = lifecycle::create_room(&state, user.id, &body.name.0).await?;
    Ok((StatusCode::CREATED, Json(RoomOut::new(room, Role::Owner))))
}

async fn rename_room(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
    JsonBody(body): JsonBody<RoomNameBody>,
) -> Result<Json<RoomOut>, ApiError> {
    let room = lifecycle::rename_room(&state, &room_id, user.id, &body.name.0).await?;
    Ok(Json(RoomOut::new(room, Role::Owner)))
}

async fn delete_room(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
) -> Result<StatusCode, ApiError> {
    lifecycle::delete_room(&state, &room_id, user.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_members(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
) -> Result<Json<Vec<MemberOut>>, ApiError> {
    membership(&state, &room_id, user.id).await?;
    let members = db::list_members(&state.pool, &room_id)
        .await?
        .into_iter()
        .map(|(member, role)| MemberOut {
            user_id: member.id,
            email: member.email,
            role: role.as_str(),
        })
        .collect();
    Ok(Json(members))
}

async fn set_member_role(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, member)): ApiPath<(String, Uuid)>,
    JsonBody(body): JsonBody<RoleBody>,
) -> Result<StatusCode, ApiError> {
    lifecycle::set_member_role(&state, &room_id, user.id, member, body.role.into()).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_member(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, member)): ApiPath<(String, Uuid)>,
) -> Result<StatusCode, ApiError> {
    lifecycle::remove_member(&state, &room_id, user.id, member).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn transfer_ownership(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
    JsonBody(body): JsonBody<OwnerBody>,
) -> Result<StatusCode, ApiError> {
    lifecycle::transfer_ownership(&state, &room_id, user.id, body.user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn membership(state: &AppState, room_id: &str, user: Uuid) -> Result<Role, ApiError> {
    db::role_in(&state.pool, room_id, user)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Room {room_id} not found")))
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
    membership(&state, &room_id, user.id).await?;
    let members = db::list_members(&state.pool, &room_id).await?;
    Ok(Json(RoomUsers {
        users: members
            .into_iter()
            .map(|(member, _)| member.email)
            .collect(),
    }))
}
