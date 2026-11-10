//! The JSON API under `/api`, plus the two legacy room routes the old UI still calls.
//! Every error is `{"detail": "<message>"}`, as FastAPI sends it.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{FromRequest, FromRequestParts, Path, Request, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use cocat_core::role::Role;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::accounts::AccountError;
use crate::db::{self, DbError, RoomRow};
use crate::lifecycle::{self, LifecycleError};
use crate::openapi::Detail;
use crate::room::RoomError;
use crate::{AppState, auth};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(me))
        .routes(routes!(list_rooms, create_room))
        .routes(routes!(rename_room, delete_room))
        .routes(routes!(list_members))
        .routes(routes!(set_member_role, remove_member))
        .routes(routes!(transfer_ownership))
        .routes(routes!(set_editors_can_share))
        .routes(routes!(legacy_rooms))
        .routes(routes!(legacy_room_users))
        .merge(crate::blobs::router())
        .merge(crate::catalogues_api::router())
        .merge(crate::documents::router())
        .merge(crate::history_api::router())
        .merge(crate::home_api::router())
        .merge(crate::groups_api::router())
        .merge(crate::invitations_api::router())
        .merge(crate::public_api::router())
        .merge(crate::submissions_api::router())
        .merge(crate::tokens_api::router())
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
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
        let id = auth::authenticate(&state.pool, &parts.headers)
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
    Unavailable(String),
    TooLarge(String),
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
            Self::Unavailable(detail) => (StatusCode::SERVICE_UNAVAILABLE, detail),
            Self::TooLarge(detail) => (StatusCode::PAYLOAD_TOO_LARGE, detail),
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
            AccountError::WeakPassword(refusal) => Self::BadRequest(refusal.0),
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

impl From<RoomError> for ApiError {
    fn from(error: RoomError) -> Self {
        match error {
            RoomError::OverBudget { .. } => {
                Self::Unavailable("Server memory budget reached".to_owned())
            }
            RoomError::Full => Self::Conflict("The project reached its storage limit".to_owned()),
            other => {
                tracing::error!(%other, "the room could not be read");
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

#[derive(Debug, Serialize, utoipa::ToSchema)]
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

/// A room in the caller's list, which also says whether it is the caller's own project.
#[derive(Debug, Serialize, utoipa::ToSchema)]
struct ListedRoom {
    id: String,
    name: String,
    role: &'static str,
    /// The caller's own project, made with the account: clients open it by default.
    personal: bool,
    /// Whether the project's editors may share it as its owner does.
    editors_can_share: bool,
    /// Whether the project has its file list, so holds a SciQLop workspace's files.
    files: bool,
    /// Deprecated: `workspace` when `files` is true, else `catalogues`. Dropped in a later release.
    kind: db::ProjectKind,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct MemberOut {
    user_id: Uuid,
    email: String,
    role: &'static str,
}

/// FastAPI's `Field(min_length=1, max_length=200)` counts characters, and a name outside
/// that range is a schema error: validating in the deserializer makes it the same 422.
/// Rooms and groups share the rule.
#[derive(Debug, Deserialize)]
#[serde(try_from = "String")]
pub(crate) struct Name(pub(crate) String);

impl TryFrom<String> for Name {
    type Error = &'static str;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        match name.chars().count() {
            1..=200 => Ok(Self(name)),
            _ => Err("the name must be 1 to 200 characters"),
        }
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct NameBody {
    #[schema(
        value_type = String,
        min_length = 1,
        max_length = 200,
        pattern = "^(?!\\s*$)[^\\u0000-\\u001F\\u007F]+$"
    )]
    pub(crate) name: Name,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(rename_all = "lowercase")]
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

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct RoleBody {
    role: AssignableRole,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct SharingBody {
    allowed: bool,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct OwnerBody {
    user_id: Uuid,
}

#[utoipa::path(
    get,
    path = "/api/me",
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The caller's account", body = Me),
        (status = 401, description = "No valid session", body = Detail),
    )
)]
async fn me(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Me>, ApiError> {
    let is_curator = db::is_curator(&state.pool, user.id).await?;
    Ok(Json(Me {
        user: user.into(),
        is_curator,
    }))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct Me {
    #[serde(flatten)]
    user: UserRead,
    /// Curators keep the public catalogues and review what people submit to them.
    is_curator: bool,
}

#[utoipa::path(
    get,
    path = "/api/rooms",
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The caller's rooms", body = [ListedRoom]),
        (status = 401, description = "No valid session", body = Detail),
    )
)]
async fn list_rooms(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Vec<ListedRoom>>, ApiError> {
    let own = db::personal_room(&state.pool, user.id).await?;
    let closed = db::rooms_closed_to_editors(&state.pool, user.id).await?;
    let mut rooms = Vec::new();
    for (room, role) in db::list_user_rooms(&state.pool, user.id).await? {
        // simplify: one query per project; fetch the file lists in the listing's own query
        // if a user ever belongs to thousands.
        let files = db::has_files(&state.pool, &room.id).await?;
        rooms.push(ListedRoom {
            personal: own.as_deref() == Some(room.id.as_str()),
            editors_can_share: !closed.contains(&room.id),
            id: room.id,
            name: room.name,
            role: role.as_str(),
            files,
            kind: kind_of(files),
        });
    }
    Ok(Json(rooms))
}

#[utoipa::path(
    post,
    path = "/api/rooms",
    request_body = NewRoomBody,
    security(("cookie" = [])),
    responses(
        (status = 201, description = "The room, with the caller as owner", body = CreatedRoom),
        (status = 400, description = "The name has control characters", body = Detail),
        (status = 401, description = "No valid session", body = Detail),
        (status = 422, description = "The name is empty or too long", body = Detail),
    )
)]
async fn create_room(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    JsonBody(body): JsonBody<NewRoomBody>,
) -> Result<(StatusCode, Json<CreatedRoom>), ApiError> {
    let kind = body.kind.unwrap_or(db::ProjectKind::Catalogues);
    let room = lifecycle::create_project(&state, user.id, &body.name.0, kind).await?;
    let files = kind == db::ProjectKind::Workspace;
    let created = CreatedRoom {
        room: RoomOut::new(room, Role::Owner),
        files,
        kind: kind_of(files),
    };
    Ok((StatusCode::CREATED, Json(created)))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct NewRoomBody {
    #[schema(
        value_type = String,
        min_length = 1,
        max_length = 200,
        pattern = "^(?!\\s*$)[^\\u0000-\\u001F\\u007F]+$"
    )]
    name: Name,
    /// `workspace` makes the project with its file list; `catalogues` (the default) without.
    kind: Option<db::ProjectKind>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct CreatedRoom {
    #[serde(flatten)]
    room: RoomOut,
    files: bool,
    /// Deprecated: see `files`.
    kind: db::ProjectKind,
}

fn kind_of(files: bool) -> db::ProjectKind {
    match files {
        true => db::ProjectKind::Workspace,
        false => db::ProjectKind::Catalogues,
    }
}

#[utoipa::path(
    patch,
    path = "/api/rooms/{room_id}",
    params(("room_id" = String, Path, description = "Project id (a room in the API)")),
    request_body = NameBody,
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The renamed room", body = RoomOut),
        (status = 400, description = "The name has control characters", body = Detail),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller is a viewer", body = Detail),
        (status = 404, description = "No such room", body = Detail),
        (status = 422, description = "The name is empty or too long", body = Detail),
    )
)]
async fn rename_room(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
    JsonBody(body): JsonBody<NameBody>,
) -> Result<Json<RoomOut>, ApiError> {
    let (room, role) = lifecycle::rename_room(&state, &room_id, user.id, &body.name.0).await?;
    Ok(Json(RoomOut::new(room, role)))
}

#[utoipa::path(
    delete,
    path = "/api/rooms/{room_id}",
    params(("room_id" = String, Path, description = "Project id (a room in the API)")),
    security(("cookie" = [])),
    responses(
        (status = 204, description = "The room is gone"),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller is not the owner", body = Detail),
        (status = 404, description = "No such room", body = Detail),
    )
)]
async fn delete_room(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
) -> Result<StatusCode, ApiError> {
    lifecycle::delete_room(&state, &room_id, user.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/rooms/{room_id}/members",
    params(("room_id" = String, Path, description = "Project id (a room in the API)")),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The members and their roles", body = [MemberOut]),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such room, or the caller is not a member", body = Detail),
        (status = 409, description = "A public catalogue has no members", body = Detail),
    )
)]
async fn list_members(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
) -> Result<Json<Vec<MemberOut>>, ApiError> {
    lifecycle::refuse_public(&state, &room_id).await?;
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

#[utoipa::path(
    patch,
    path = "/api/rooms/{room_id}/members/{user_id}",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("user_id" = Uuid, Path, description = "Member's account id"),
    ),
    request_body = RoleBody,
    security(("cookie" = [])),
    responses(
        (status = 204, description = "The role is set"),
        (status = 400, description = "The role cannot be assigned", body = Detail),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller may not share the project", body = Detail),
        (status = 404, description = "No such room or member", body = Detail),
        (status = 409, description = "The owner keeps the owner role", body = Detail),
        (status = 422, description = "The body is not a role", body = Detail),
    )
)]
async fn set_member_role(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, member)): ApiPath<(String, Uuid)>,
    JsonBody(body): JsonBody<RoleBody>,
) -> Result<StatusCode, ApiError> {
    lifecycle::set_member_role(&state, &room_id, user.id, member, body.role.into()).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/api/rooms/{room_id}/members/{user_id}",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("user_id" = Uuid, Path, description = "Member's account id"),
    ),
    security(("cookie" = [])),
    responses(
        (status = 204, description = "The membership is gone"),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller may not share the project", body = Detail),
        (status = 404, description = "No such room or member", body = Detail),
        (status = 409, description = "The owner cannot leave or be removed", body = Detail),
        (status = 422, description = "The user id is not an account id", body = Detail),
    )
)]
async fn remove_member(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, member)): ApiPath<(String, Uuid)>,
) -> Result<StatusCode, ApiError> {
    lifecycle::remove_member(&state, &room_id, user.id, member).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/rooms/{room_id}/owner",
    params(("room_id" = String, Path, description = "Project id (a room in the API)")),
    request_body = OwnerBody,
    security(("cookie" = [])),
    responses(
        (status = 204, description = "The room has a new owner"),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller is not the owner", body = Detail),
        (status = 404, description = "No such room or account", body = Detail),
        (status = 409, description = "The project is its owner's own (“My catalogues”)", body = Detail),
        (status = 422, description = "The body is not an account id", body = Detail),
    )
)]
async fn transfer_ownership(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
    JsonBody(body): JsonBody<OwnerBody>,
) -> Result<StatusCode, ApiError> {
    lifecycle::transfer_ownership(&state, &room_id, user.id, body.user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Lets the project's editors share it as its owner does (`true`, the default), or keeps
/// sharing to the owner (`false`).
#[utoipa::path(
    put,
    path = "/api/rooms/{room_id}/editors-can-share",
    params(("room_id" = String, Path, description = "Project id (a room in the API)")),
    request_body = SharingBody,
    security(("cookie" = [])),
    responses(
        (status = 204, description = "The setting is saved"),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller is not the owner", body = Detail),
        (status = 404, description = "No such room", body = Detail),
        (status = 422, description = "The body is not a setting", body = Detail),
    )
)]
async fn set_editors_can_share(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
    JsonBody(body): JsonBody<SharingBody>,
) -> Result<StatusCode, ApiError> {
    lifecycle::set_editors_can_share(&state, &room_id, user.id, body.allowed).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn membership(
    state: &AppState,
    room_id: &str,
    user: Uuid,
) -> Result<Role, ApiError> {
    db::role_in(&state.pool, room_id, user)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Project {room_id} not found")))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct UserRooms {
    rooms: Vec<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RoomUsers {
    users: Vec<String>,
}

/// `GET /rooms`: the ids of the caller's rooms, for the old UI.
#[utoipa::path(
    get,
    path = "/rooms",
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The caller's room ids", body = UserRooms),
        (status = 401, description = "No valid session", body = Detail),
    )
)]
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
#[utoipa::path(
    get,
    path = "/room/{room_id}/users",
    params(("room_id" = String, Path, description = "Project id (a room in the API)")),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The members' addresses", body = RoomUsers),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such room, or the caller is not a member", body = Detail),
    )
)]
pub async fn legacy_room_users(
    Path(room_id): Path<String>,
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<RoomUsers>, ApiError> {
    lifecycle::refuse_public(&state, &room_id).await?;
    membership(&state, &room_id, user.id).await?;
    let members = db::list_members(&state.pool, &room_id).await?;
    Ok(Json(RoomUsers {
        users: members
            .into_iter()
            .map(|(member, _)| member.email)
            .collect(),
    }))
}
