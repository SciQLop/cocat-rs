//! The invitation routes, kept out of `api` so neither file grows past its one job:
//! `api` owns rooms and members, `invitations` owns the rules, this module the HTTP shape.

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, ApiPath, AssignableRole, CurrentUser, JsonBody, RoomOut};
use crate::lifecycle::LifecycleError;
use crate::mail::{Address, is_placeholder};
use crate::openapi::Detail;
use crate::{AppState, invitations};

const INVITATION_LIFETIME_DAYS: u32 = 14;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/rooms/{room_id}/invitations",
            get(list_invitations).post(create_invitation),
        )
        .route(
            "/rooms/{room_id}/invitations/{invitation_id}",
            delete(revoke_invitation),
        )
        .route("/invitations/preview", post(preview_invitation))
        .route("/invitations/accept", post(accept_invitation))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct InvitationCreate {
    #[schema(value_type = Option<String>, format = Email)]
    email: Option<Address>,
    #[serde(default = "default_role")]
    role: AssignableRole,
    #[serde(default = "default_expiry")]
    #[schema(value_type = u32, minimum = 1, maximum = 90)]
    expires_in_days: ExpiresInDays,
    #[serde(default)]
    #[schema(value_type = Option<u32>, minimum = 1)]
    max_uses: Option<MaxUses>,
}

fn default_role() -> AssignableRole {
    AssignableRole::Editor
}

pub(crate) fn default_expiry() -> ExpiresInDays {
    ExpiresInDays(INVITATION_LIFETIME_DAYS)
}

/// FastAPI's `Field(14, ge=1, le=90)`: a schema error, so it is refused while deserializing
/// and reaches the client as the same 422 as any other invalid body.
#[derive(Debug, Deserialize)]
#[serde(try_from = "u32")]
pub(crate) struct ExpiresInDays(pub(crate) u32);

impl TryFrom<u32> for ExpiresInDays {
    type Error = &'static str;

    fn try_from(days: u32) -> Result<Self, Self::Error> {
        match days {
            1..=90 => Ok(Self(days)),
            _ => Err("expires_in_days must be 1 to 90"),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(try_from = "u32")]
pub(crate) struct MaxUses(pub(crate) u32);

impl TryFrom<u32> for MaxUses {
    type Error = &'static str;

    fn try_from(uses: u32) -> Result<Self, Self::Error> {
        match uses {
            0 => Err("max_uses must be at least 1"),
            uses => Ok(Self(uses)),
        }
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct TokenBody {
    pub(crate) token: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct InvitationCreated {
    id: i64,
    token: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct InvitationOut {
    id: i64,
    email: Option<String>,
    role: &'static str,
    expires_at: String,
    uses: i64,
    max_uses: Option<i64>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct PreviewOut {
    room_id: String,
    room_name: String,
    role: &'static str,
    inviter_email: Option<String>,
    expires_at: String,
}

#[utoipa::path(
    post,
    path = "/api/rooms/{room_id}/invitations",
    params(("room_id" = String, Path, description = "Room id")),
    request_body = InvitationCreate,
    security(("cookie" = [])),
    responses(
        (status = 201, description = "The new invitation and its token", body = InvitationCreated),
        (status = 400, description = "The invitation was refused", body = Detail),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller may not invite", body = Detail),
        (status = 404, description = "No such room", body = Detail),
        (status = 422, description = "The body is not an invitation", body = Detail),
    )
)]
async fn create_invitation(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
    JsonBody(body): JsonBody<InvitationCreate>,
) -> Result<(StatusCode, Json<InvitationCreated>), ApiError> {
    let email = body.email.map(address).transpose()?;
    let (id, token) = invitations::create(
        &state,
        &room_id,
        user.id,
        body.role.into(),
        email.as_deref(),
        body.expires_in_days.0,
        body.max_uses.map(|uses| uses.0),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(InvitationCreated { id, token })))
}

#[utoipa::path(
    get,
    path = "/api/rooms/{room_id}/invitations",
    params(("room_id" = String, Path, description = "Room id")),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The room's live invitations", body = [InvitationOut]),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller may not see the invitations", body = Detail),
        (status = 404, description = "No such room", body = Detail),
    )
)]
async fn list_invitations(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
) -> Result<Json<Vec<InvitationOut>>, ApiError> {
    let invitations = invitations::list(&state, &room_id, user.id)
        .await?
        .into_iter()
        .map(|invitation| InvitationOut {
            id: invitation.id,
            email: invitation.email,
            role: invitation.role.as_str(),
            expires_at: invitation.expires_at,
            uses: invitation.uses,
            max_uses: invitation.max_uses,
        })
        .collect();
    Ok(Json(invitations))
}

#[utoipa::path(
    delete,
    path = "/api/rooms/{room_id}/invitations/{invitation_id}",
    params(
        ("room_id" = String, Path, description = "Room id"),
        ("invitation_id" = i64, Path, description = "Invitation id"),
    ),
    security(("cookie" = [])),
    responses(
        (status = 204, description = "The invitation is revoked"),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller may not revoke it", body = Detail),
        (status = 404, description = "No such room or invitation", body = Detail),
    )
)]
async fn revoke_invitation(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, invitation_id)): ApiPath<(String, i64)>,
) -> Result<StatusCode, ApiError> {
    invitations::revoke(&state, &room_id, user.id, invitation_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// No cookie: the guest has to see what the link offers before signing in.
#[utoipa::path(
    post,
    path = "/api/invitations/preview",
    request_body = TokenBody,
    responses(
        (status = 200, description = "The room the token invites to", body = PreviewOut),
        (status = 400, description = "The token is spent or not ours", body = Detail),
        (status = 422, description = "The body has no token", body = Detail),
    )
)]
async fn preview_invitation(
    State(state): State<AppState>,
    JsonBody(body): JsonBody<TokenBody>,
) -> Result<Json<PreviewOut>, ApiError> {
    let preview = invitations::preview(&state, &body.token).await?;
    Ok(Json(PreviewOut {
        room_id: preview.room_id,
        room_name: preview.room_name,
        role: preview.role.as_str(),
        inviter_email: preview.inviter_email,
        expires_at: preview.expires_at,
    }))
}

#[utoipa::path(
    post,
    path = "/api/invitations/accept",
    request_body = TokenBody,
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The room, joined", body = RoomOut),
        (status = 400, description = "The token is spent or not ours", body = Detail),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The invitation does not admit this account", body = Detail),
        (status = 422, description = "The body has no token", body = Detail),
    )
)]
async fn accept_invitation(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    JsonBody(body): JsonBody<TokenBody>,
) -> Result<Json<RoomOut>, ApiError> {
    let (room, role) = invitations::accept(&state, &body.token, &user).await?;
    Ok(Json(RoomOut::new(room, role)))
}

/// Python's `actions._address` once `EmailStr` has accepted the address: the only address it
/// still refuses is a placeholder of ours, which exists to stand in for an account but must
/// never receive an invitation.
pub(crate) fn address(email: Address) -> Result<String, ApiError> {
    if is_placeholder(&email.0) {
        return Err(LifecycleError::Invalid("Enter a valid email address").into());
    }
    Ok(email.0)
}

/// The invitation routes as the OpenAPI document lists them; the live router is `router`.
pub(crate) fn documented() -> utoipa_axum::router::OpenApiRouter<AppState> {
    utoipa_axum::router::OpenApiRouter::new()
        .routes(utoipa_axum::routes!(list_invitations, create_invitation))
        .routes(utoipa_axum::routes!(revoke_invitation))
        .routes(utoipa_axum::routes!(preview_invitation))
        .routes(utoipa_axum::routes!(accept_invitation))
}
