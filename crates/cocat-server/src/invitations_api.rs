//! The invitation routes, kept out of `api` so neither file grows past its one job:
//! `api` owns rooms and members, `invitations` owns the rules, this module the HTTP shape.

use std::str::FromStr;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use email_address::EmailAddress;
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, ApiPath, AssignableRole, CurrentUser, JsonBody, RoomOut};
use crate::lifecycle::LifecycleError;
use crate::{AppState, invitations};

const PLACEHOLDER_DOMAIN: &str = "placeholder.cocat.org";
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

#[derive(Debug, Deserialize)]
struct InvitationCreate {
    email: Option<String>,
    #[serde(default = "default_role")]
    role: AssignableRole,
    #[serde(default = "default_expiry")]
    expires_in_days: ExpiresInDays,
    #[serde(default)]
    max_uses: Option<MaxUses>,
}

fn default_role() -> AssignableRole {
    AssignableRole::Editor
}

fn default_expiry() -> ExpiresInDays {
    ExpiresInDays(INVITATION_LIFETIME_DAYS)
}

/// FastAPI's `Field(14, ge=1, le=90)`: a schema error, so it is refused while deserializing
/// and reaches the client as the same 422 as any other invalid body.
#[derive(Debug, Deserialize)]
#[serde(try_from = "u32")]
struct ExpiresInDays(u32);

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
struct MaxUses(u32);

impl TryFrom<u32> for MaxUses {
    type Error = &'static str;

    fn try_from(uses: u32) -> Result<Self, Self::Error> {
        match uses {
            0 => Err("max_uses must be at least 1"),
            uses => Ok(Self(uses)),
        }
    }
}

#[derive(Debug, Deserialize)]
struct TokenBody {
    token: String,
}

#[derive(Debug, Serialize)]
struct InvitationCreated {
    id: i64,
    token: String,
}

#[derive(Debug, Serialize)]
struct InvitationOut {
    id: i64,
    email: Option<String>,
    role: &'static str,
    expires_at: String,
    uses: i64,
    max_uses: Option<i64>,
}

#[derive(Debug, Serialize)]
struct PreviewOut {
    room_id: String,
    room_name: String,
    role: &'static str,
    inviter_email: Option<String>,
    expires_at: String,
}

async fn create_invitation(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
    JsonBody(body): JsonBody<InvitationCreate>,
) -> Result<(StatusCode, Json<InvitationCreated>), ApiError> {
    let email = body.email.as_deref().map(address).transpose()?;
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

async fn revoke_invitation(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, invitation_id)): ApiPath<(String, i64)>,
) -> Result<StatusCode, ApiError> {
    invitations::revoke(&state, &room_id, user.id, invitation_id)
        .await
        .map_err(invitation_not_found)?;
    Ok(StatusCode::NO_CONTENT)
}

/// No cookie: the guest has to see what the link offers before signing in.
async fn preview_invitation(
    State(state): State<AppState>,
    JsonBody(body): JsonBody<TokenBody>,
) -> Result<Json<PreviewOut>, ApiError> {
    let preview = invitations::preview(&state, &body.token)
        .await
        .map_err(invitation_not_found)?;
    Ok(Json(PreviewOut {
        room_id: preview.room_id,
        room_name: preview.room_name,
        role: preview.role.as_str(),
        inviter_email: preview.inviter_email,
        expires_at: preview.expires_at,
    }))
}

async fn accept_invitation(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    JsonBody(body): JsonBody<TokenBody>,
) -> Result<Json<RoomOut>, ApiError> {
    let (room, role) = invitations::accept(&state, &body.token, &user)
        .await
        .map_err(invitation_not_found)?;
    Ok(Json(RoomOut::new(room, role)))
}

/// An unknown, expired, revoked or spent token is the same 404, as in Python.
fn invitation_not_found(error: LifecycleError) -> ApiError {
    match error {
        LifecycleError::NotFound => ApiError::NotFound("Invitation not found".to_owned()),
        other => other.into(),
    }
}

/// The address to invite, or Python's refusal: blank, a placeholder of ours, or not an
/// address at all.
fn address(email: &str) -> Result<String, ApiError> {
    let trimmed = email.trim();
    if trimmed.is_empty() || is_placeholder(trimmed) || EmailAddress::from_str(trimmed).is_err() {
        return Err(LifecycleError::Invalid("Enter a valid email address").into());
    }
    Ok(trimmed.to_owned())
}

/// `mail.py::is_placeholder`: the reserved domain and its provider subdomains.
fn is_placeholder(email: &str) -> bool {
    let Some((_, domain)) = email.rsplit_once('@') else {
        return false;
    };
    let domain = domain.to_ascii_lowercase();
    domain == PLACEHOLDER_DOMAIN || domain.ends_with(&format!(".{PLACEHOLDER_DOMAIN}"))
}
