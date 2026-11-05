//! The group routes, kept out of `api` the way `invitations_api` is: the rules live in
//! `groups`, this module only owns the HTTP shape.

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{delete, get, patch, post, put};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::api::{ApiError, ApiPath, AssignableRole, CurrentUser, JsonBody, Name};
use cocat_core::role::Role;

use crate::groups::{self, GroupMember, GroupRole, GroupRow};
use crate::invitations_api::{self, ExpiresInDays, MaxUses, TokenBody};
use crate::mail::Address;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/groups", get(list).post(create))
        .route("/groups/{group_id}", patch(rename).delete(remove))
        .route("/groups/{group_id}/members", get(members))
        .route(
            "/groups/{group_id}/members/{user_id}",
            delete(remove_member),
        )
        .route(
            "/groups/{group_id}/invitations",
            get(list_invitations).post(create_invitation),
        )
        .route(
            "/groups/{group_id}/invitations/{invitation_id}",
            delete(revoke_invitation),
        )
        .route("/group-invitations/preview", post(preview_invitation))
        .route("/group-invitations/accept", post(accept_invitation))
        .route("/rooms/{room_id}/groups", get(room_groups))
        .route(
            "/rooms/{room_id}/groups/{group_id}",
            put(grant).delete(ungrant),
        )
}

#[derive(Debug, Deserialize)]
struct GrantBody {
    role: AssignableRole,
}

#[derive(Debug, Deserialize)]
struct NameBody {
    name: Name,
}

#[derive(Debug, Serialize)]
struct GroupOut {
    id: String,
    name: String,
    role: &'static str,
}

impl GroupOut {
    fn new(row: GroupRow, role: &'static str) -> Self {
        Self {
            id: row.id,
            name: row.name,
            role,
        }
    }
}

#[derive(Debug, Serialize)]
struct GroupsOut {
    groups: Vec<GroupOut>,
}

#[derive(Debug, Serialize)]
struct MemberOut {
    user_id: Uuid,
    email: String,
    role: &'static str,
}

impl From<GroupMember> for MemberOut {
    fn from(member: GroupMember) -> Self {
        Self {
            user_id: member.user_id,
            email: member.email,
            role: member.role.as_str(),
        }
    }
}

#[derive(Debug, Serialize)]
struct MembersOut {
    members: Vec<MemberOut>,
}

#[derive(Debug, Deserialize)]
struct GroupInvitationCreate {
    email: Option<Address>,
    #[serde(default = "crate::invitations_api::default_expiry")]
    expires_in_days: ExpiresInDays,
    #[serde(default)]
    max_uses: Option<MaxUses>,
}

#[derive(Debug, Serialize)]
struct InvitationCreated {
    id: i64,
    token: String,
}

#[derive(Debug, Serialize)]
struct GroupInvitationOut {
    id: i64,
    email: Option<String>,
    expires_at: String,
    uses: i64,
    max_uses: Option<i64>,
}

#[derive(Debug, Serialize)]
struct GroupPreviewOut {
    group_id: String,
    group_name: String,
    inviter_email: Option<String>,
    expires_at: String,
}

async fn list(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<GroupsOut>, ApiError> {
    let groups = groups::list_for(&state, user.id)
        .await?
        .into_iter()
        .map(|(row, role)| GroupOut::new(row, role.as_str()))
        .collect();
    Ok(Json(GroupsOut { groups }))
}

async fn create(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    JsonBody(body): JsonBody<NameBody>,
) -> Result<(StatusCode, Json<GroupOut>), ApiError> {
    let row = groups::create(&state, user.id, &body.name.0).await?;
    Ok((
        StatusCode::CREATED,
        Json(GroupOut::new(row, GroupRole::Owner.as_str())),
    ))
}

async fn rename(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(group_id): ApiPath<String>,
    JsonBody(body): JsonBody<NameBody>,
) -> Result<Json<GroupOut>, ApiError> {
    let row = groups::rename(&state, &group_id, user.id, &body.name.0).await?;
    Ok(Json(GroupOut::new(row, GroupRole::Owner.as_str())))
}

async fn remove(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(group_id): ApiPath<String>,
) -> Result<StatusCode, ApiError> {
    groups::delete(&state, &group_id, user.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn members(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(group_id): ApiPath<String>,
) -> Result<Json<MembersOut>, ApiError> {
    let members = groups::members(&state, &group_id, user.id)
        .await?
        .into_iter()
        .map(MemberOut::from)
        .collect();
    Ok(Json(MembersOut { members }))
}

async fn remove_member(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((group_id, member)): ApiPath<(String, Uuid)>,
) -> Result<StatusCode, ApiError> {
    groups::remove_member(&state, &group_id, user.id, member).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn room_groups(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
) -> Result<Json<GroupsOut>, ApiError> {
    let groups = groups::room_groups(&state, &room_id, user.id)
        .await?
        .into_iter()
        .map(|(row, role)| GroupOut::new(row, role.as_str()))
        .collect();
    Ok(Json(GroupsOut { groups }))
}

async fn grant(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, group_id)): ApiPath<(String, String)>,
    JsonBody(body): JsonBody<GrantBody>,
) -> Result<Json<GroupOut>, ApiError> {
    let role = Role::from(body.role);
    let (row, granted) = groups::grant(&state, &room_id, &group_id, user.id, role).await?;
    Ok(Json(GroupOut::new(row, granted.as_str())))
}

async fn ungrant(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, group_id)): ApiPath<(String, String)>,
) -> Result<StatusCode, ApiError> {
    groups::ungrant(&state, &room_id, &group_id, user.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_invitation(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(group_id): ApiPath<String>,
    JsonBody(body): JsonBody<GroupInvitationCreate>,
) -> Result<(StatusCode, Json<InvitationCreated>), ApiError> {
    let email = body.email.map(invitations_api::address).transpose()?;
    let (id, token) = groups::create_invitation(
        &state,
        &group_id,
        user.id,
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
    ApiPath(group_id): ApiPath<String>,
) -> Result<Json<Vec<GroupInvitationOut>>, ApiError> {
    let invitations = groups::list_invitations(&state, &group_id, user.id)
        .await?
        .into_iter()
        .map(|invitation| GroupInvitationOut {
            id: invitation.id,
            email: invitation.email,
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
    ApiPath((group_id, invitation_id)): ApiPath<(String, i64)>,
) -> Result<StatusCode, ApiError> {
    groups::revoke_invitation(&state, &group_id, user.id, invitation_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn preview_invitation(
    State(state): State<AppState>,
    JsonBody(body): JsonBody<TokenBody>,
) -> Result<Json<GroupPreviewOut>, ApiError> {
    let preview = groups::preview_invitation(&state, &body.token).await?;
    Ok(Json(GroupPreviewOut {
        group_id: preview.group_id,
        group_name: preview.group_name,
        inviter_email: preview.inviter_email,
        expires_at: preview.expires_at,
    }))
}

async fn accept_invitation(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    JsonBody(body): JsonBody<TokenBody>,
) -> Result<Json<GroupOut>, ApiError> {
    let group = groups::accept_invitation(&state, &body.token, &user).await?;
    Ok(Json(GroupOut::new(group, GroupRole::Member.as_str())))
}
