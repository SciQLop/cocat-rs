//! The invitation routes, kept out of `api` so neither file grows past its one job:
//! `api` owns rooms and members, `invitations` owns the rules, this module the HTTP shape.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::{ApiError, ApiPath, AssignableRole, CurrentUser, JsonBody, RoomOut};
use crate::lifecycle::LifecycleError;
use crate::lifecycle::require_sharer;
use crate::mail::{Address, is_placeholder};
use crate::openapi::Detail;
use crate::{AppState, db, invitations};

const INVITATION_LIFETIME_DAYS: u32 = 14;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_invitations, create_invitation))
        .routes(routes!(revoke_invitation))
        .routes(routes!(suggestions))
        .routes(routes!(preview_invitation))
        .routes(routes!(accept_invitation))
        .routes(routes!(accept_addressed))
        .routes(routes!(decline_addressed))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct InvitationCreate {
    #[schema(value_type = Option<String>, format = Email, min_length = 1)]
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
#[serde(try_from = "Count")]
pub(crate) struct ExpiresInDays(pub(crate) u32);

impl TryFrom<Count> for ExpiresInDays {
    type Error = &'static str;

    fn try_from(count: Count) -> Result<Self, Self::Error> {
        match count.whole()? {
            days @ 1..=90 => Ok(Self(days)),
            _ => Err("expires_in_days must be 1 to 90"),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(try_from = "Count")]
pub(crate) struct MaxUses(pub(crate) u32);

impl TryFrom<Count> for MaxUses {
    type Error = &'static str;

    fn try_from(count: Count) -> Result<Self, Self::Error> {
        match count.whole()? {
            0 => Err("max_uses must be at least 1"),
            uses => Ok(Self(uses)),
        }
    }
}

/// JSON Schema counts `7.0` as an integer and Python's pydantic coerces it, so a number
/// without a fractional part is accepted whichever way the client wrote it.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Count {
    Integer(u64),
    Float(f64),
}

impl Count {
    fn whole(self) -> Result<u32, &'static str> {
        match self {
            Self::Integer(value) => u32::try_from(value).map_err(|_| "the count is too large"),
            Self::Float(value)
                if value.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(&value) =>
            {
                Ok(value as u32)
            }
            Self::Float(_) => Err("the count must be a whole number"),
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
pub(crate) struct PreviewOut {
    room_id: String,
    room_name: String,
    role: &'static str,
    inviter_email: Option<String>,
    expires_at: String,
}

impl From<invitations::Preview> for PreviewOut {
    fn from(preview: invitations::Preview) -> Self {
        Self {
            room_id: preview.room_id,
            room_name: preview.room_name,
            role: preview.role.as_str(),
            inviter_email: preview.inviter_email,
            expires_at: preview.expires_at,
        }
    }
}

#[utoipa::path(
    post,
    path = "/api/rooms/{room_id}/invitations",
    params(("room_id" = String, Path, description = "Project id (a room in the API)")),
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
    params(("room_id" = String, Path, description = "Project id (a room in the API)")),
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
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("invitation_id" = i64, Path, description = "Invitation id"),
    ),
    security(("cookie" = [])),
    responses(
        (status = 204, description = "The invitation is revoked"),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller may not revoke it", body = Detail),
        (status = 404, description = "No such room or invitation", body = Detail),
        (status = 422, description = "The invitation id is not a number", body = Detail),
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

/// What the invitation form suggests to whoever may share: the people they work with who are not members.
#[utoipa::path(
    get,
    path = "/api/rooms/{room_id}/suggestions",
    params(("room_id" = String, Path, description = "Project id (a room in the API)")),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "Addresses to suggest, by address", body = [String]),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller may not invite", body = Detail),
        (status = 404, description = "No such room", body = Detail),
    )
)]
async fn suggestions(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
) -> Result<Json<Vec<String>>, ApiError> {
    require_sharer(&state, &room_id, user.id).await?;
    let members = db::list_members(&state.pool, &room_id).await?;
    let emails: Vec<&str> = members
        .iter()
        .map(|(member, _)| member.email.as_str())
        .collect();
    Ok(Json(
        invitations::suggestions(&state, user.id, &emails).await?,
    ))
}

/// No cookie: the guest has to see what the link offers before signing in.
#[utoipa::path(
    post,
    path = "/api/invitations/preview",
    request_body = TokenBody,
    responses(
        (status = 200, description = "The room the token invites to", body = PreviewOut),
        (status = 400, description = "The token is spent or not ours", body = Detail),
        (status = 404, description = "No invitation carries this token", body = Detail),
        (status = 422, description = "The body has no token", body = Detail),
    )
)]
async fn preview_invitation(
    State(state): State<AppState>,
    JsonBody(body): JsonBody<TokenBody>,
) -> Result<Json<PreviewOut>, ApiError> {
    let preview = invitations::preview(&state, &body.token).await?;
    Ok(Json(preview.into()))
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
        (status = 404, description = "No invitation carries this token", body = Detail),
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

/// The home page's Accept: no token, the invitation is addressed to the caller.
#[utoipa::path(
    post,
    path = "/api/invitations/{invitation_id}/accept",
    params(("invitation_id" = i64, Path, description = "Invitation id")),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The room, joined with the invited role", body = RoomOut),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No usable invitation with this id addressed to the caller's verified email", body = Detail),
        (status = 422, description = "The invitation id is not a number", body = Detail),
    )
)]
async fn accept_addressed(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(invitation_id): ApiPath<i64>,
) -> Result<Json<RoomOut>, ApiError> {
    let (room, role) = invitations::accept_addressed(&state, invitation_id, &user).await?;
    Ok(Json(RoomOut::new(room, role)))
}

/// The home page's Decline: revokes the invitation, so nobody can use it any more.
#[utoipa::path(
    post,
    path = "/api/invitations/{invitation_id}/decline",
    params(("invitation_id" = i64, Path, description = "Invitation id")),
    security(("cookie" = [])),
    responses(
        (status = 204, description = "The invitation is revoked"),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No usable invitation with this id addressed to the caller's verified email", body = Detail),
        (status = 422, description = "The invitation id is not a number", body = Detail),
    )
)]
async fn decline_addressed(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(invitation_id): ApiPath<i64>,
) -> Result<StatusCode, ApiError> {
    invitations::decline(&state, invitation_id, &user).await?;
    Ok(StatusCode::NO_CONTENT)
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
