//! The group routes, kept out of `api` the way `invitations_api` is: the rules live in
//! `groups`, this module only owns the HTTP shape.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::api::{ApiError, ApiPath, AssignableRole, CurrentUser, JsonBody, Name, RoomOut};
use crate::{AppState, invitations};
use cocat_core::role::Role;

use crate::groups::{self, GroupMember, GroupRole, GroupRow};
use crate::invitations_api::{self, ExpiresInDays, MaxUses, TokenBody};
use crate::mail::Address;
use crate::openapi::Detail;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(rename, remove))
        .routes(routes!(members))
        .routes(routes!(suggestions))
        .routes(routes!(shared_rooms))
        .routes(routes!(remove_member))
        .routes(routes!(list_invitations, create_invitation))
        .routes(routes!(revoke_invitation))
        .routes(routes!(preview_invitation))
        .routes(routes!(accept_invitation))
        .routes(routes!(room_groups))
        .routes(routes!(grant, ungrant))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct GrantBody {
    role: AssignableRole,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct NameBody {
    #[schema(
        value_type = String,
        min_length = 1,
        max_length = 200,
        pattern = "^(?!\\s*$)[^\\u0000-\\u001F\\u007F]+$"
    )]
    name: Name,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
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

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct GroupsOut {
    groups: Vec<GroupOut>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
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

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct MembersOut {
    members: Vec<MemberOut>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct GroupInvitationCreate {
    #[schema(value_type = Option<String>, format = Email, min_length = 1)]
    email: Option<Address>,
    #[serde(default = "crate::invitations_api::default_expiry")]
    #[schema(value_type = u32, minimum = 1, maximum = 90)]
    expires_in_days: ExpiresInDays,
    #[serde(default)]
    #[schema(value_type = Option<u32>, minimum = 1)]
    max_uses: Option<MaxUses>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct InvitationCreated {
    id: i64,
    token: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct GroupInvitationOut {
    id: i64,
    email: Option<String>,
    expires_at: String,
    uses: i64,
    max_uses: Option<i64>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct GroupPreviewOut {
    group_id: String,
    group_name: String,
    inviter_email: Option<String>,
    expires_at: String,
}

#[utoipa::path(
    get,
    path = "/api/groups",
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The caller's groups", body = GroupsOut),
        (status = 401, description = "No valid session", body = Detail),
    )
)]
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

#[utoipa::path(
    post,
    path = "/api/groups",
    request_body = NameBody,
    security(("cookie" = [])),
    responses(
        (status = 201, description = "The new group, with the caller as owner", body = GroupOut),
        (status = 400, description = "The name has control characters", body = Detail),
        (status = 401, description = "No valid session", body = Detail),
        (status = 422, description = "The name is empty or too long", body = Detail),
    )
)]
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

#[utoipa::path(
    patch,
    path = "/api/groups/{group_id}",
    params(("group_id" = String, Path, description = "Group id")),
    request_body = NameBody,
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The renamed group", body = GroupOut),
        (status = 400, description = "The name has control characters", body = Detail),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller is not the owner", body = Detail),
        (status = 404, description = "No such group", body = Detail),
        (status = 422, description = "The name is empty or too long", body = Detail),
    )
)]
async fn rename(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(group_id): ApiPath<String>,
    JsonBody(body): JsonBody<NameBody>,
) -> Result<Json<GroupOut>, ApiError> {
    let row = groups::rename(&state, &group_id, user.id, &body.name.0).await?;
    Ok(Json(GroupOut::new(row, GroupRole::Owner.as_str())))
}

#[utoipa::path(
    delete,
    path = "/api/groups/{group_id}",
    params(("group_id" = String, Path, description = "Group id")),
    security(("cookie" = [])),
    responses(
        (status = 204, description = "The group is gone"),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller is not the owner", body = Detail),
        (status = 404, description = "No such group", body = Detail),
    )
)]
async fn remove(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(group_id): ApiPath<String>,
) -> Result<StatusCode, ApiError> {
    groups::delete(&state, &group_id, user.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/groups/{group_id}/members",
    params(("group_id" = String, Path, description = "Group id")),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The group's members and their roles", body = MembersOut),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such group, or the caller is not a member", body = Detail),
    )
)]
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

/// What the owner's invitation form suggests: the people they work with outside the group.
#[utoipa::path(
    get,
    path = "/api/groups/{group_id}/suggestions",
    params(("group_id" = String, Path, description = "Group id")),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "Addresses to suggest, by address", body = [String]),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller is not the owner", body = Detail),
        (status = 404, description = "No such group, or the caller is not a member", body = Detail),
    )
)]
async fn suggestions(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(group_id): ApiPath<String>,
) -> Result<Json<Vec<String>>, ApiError> {
    groups::require_owner(&state, &group_id, user.id).await?;
    let members = groups::members(&state, &group_id, user.id).await?;
    let emails: Vec<&str> = members.iter().map(|member| member.email.as_str()).collect();
    Ok(Json(
        invitations::suggestions(&state, user.id, &emails).await?,
    ))
}

#[utoipa::path(
    get,
    path = "/api/groups/{group_id}/rooms",
    params(("group_id" = String, Path, description = "Group id")),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The rooms shared with the group, with the role it grants", body = [RoomOut]),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such group, or the caller is not a member", body = Detail),
    )
)]
async fn shared_rooms(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(group_id): ApiPath<String>,
) -> Result<Json<Vec<RoomOut>>, ApiError> {
    let rooms = groups::shared_rooms(&state, &group_id, user.id)
        .await?
        .into_iter()
        .map(|(room, role)| RoomOut::new(room, role))
        .collect();
    Ok(Json(rooms))
}

#[utoipa::path(
    delete,
    path = "/api/groups/{group_id}/members/{user_id}",
    params(
        ("group_id" = String, Path, description = "Group id"),
        ("user_id" = Uuid, Path, description = "Member's account id"),
    ),
    security(("cookie" = [])),
    responses(
        (status = 204, description = "The membership is gone"),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller may not remove this member", body = Detail),
        (status = 404, description = "No such group or member", body = Detail),
        (status = 409, description = "The owner cannot leave the group", body = Detail),
        (status = 422, description = "The user id is not an account id", body = Detail),
    )
)]
async fn remove_member(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((group_id, member)): ApiPath<(String, Uuid)>,
) -> Result<StatusCode, ApiError> {
    groups::remove_member(&state, &group_id, user.id, member).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/rooms/{room_id}/groups",
    params(("room_id" = String, Path, description = "Project id (a room in the API)")),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The groups granted on the room", body = GroupsOut),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such room, or the caller is not a member", body = Detail),
    )
)]
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

#[utoipa::path(
    put,
    path = "/api/rooms/{room_id}/groups/{group_id}",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("group_id" = String, Path, description = "Group id"),
    ),
    request_body = GrantBody,
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The group and the role it now has on the room", body = GroupOut),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller may not grant this group", body = Detail),
        (status = 404, description = "No such room or group", body = Detail),
        (status = 409, description = "The project is its owner's own (“My catalogues”)", body = Detail),
        (status = 422, description = "The body is not a role", body = Detail),
    )
)]
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

#[utoipa::path(
    delete,
    path = "/api/rooms/{room_id}/groups/{group_id}",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("group_id" = String, Path, description = "Group id"),
    ),
    security(("cookie" = [])),
    responses(
        (status = 204, description = "The group no longer has access to the room"),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller is not the owner", body = Detail),
        (status = 404, description = "No such room or group", body = Detail),
    )
)]
async fn ungrant(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, group_id)): ApiPath<(String, String)>,
) -> Result<StatusCode, ApiError> {
    groups::ungrant(&state, &room_id, &group_id, user.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/groups/{group_id}/invitations",
    params(("group_id" = String, Path, description = "Group id")),
    request_body = GroupInvitationCreate,
    security(("cookie" = [])),
    responses(
        (status = 201, description = "The new invitation and its token", body = InvitationCreated),
        (status = 400, description = "The invitation was refused", body = Detail),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller may not invite", body = Detail),
        (status = 404, description = "No such group", body = Detail),
        (status = 422, description = "The body is not an invitation", body = Detail),
    )
)]
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

#[utoipa::path(
    get,
    path = "/api/groups/{group_id}/invitations",
    params(("group_id" = String, Path, description = "Group id")),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The group's live invitations", body = [GroupInvitationOut]),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller may not see the invitations", body = Detail),
        (status = 404, description = "No such group", body = Detail),
    )
)]
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

#[utoipa::path(
    delete,
    path = "/api/groups/{group_id}/invitations/{invitation_id}",
    params(
        ("group_id" = String, Path, description = "Group id"),
        ("invitation_id" = i64, Path, description = "Invitation id"),
    ),
    security(("cookie" = [])),
    responses(
        (status = 204, description = "The invitation is revoked"),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller may not revoke it", body = Detail),
        (status = 404, description = "No such group or invitation", body = Detail),
        (status = 422, description = "The invitation id is not a number", body = Detail),
    )
)]
async fn revoke_invitation(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((group_id, invitation_id)): ApiPath<(String, i64)>,
) -> Result<StatusCode, ApiError> {
    groups::revoke_invitation(&state, &group_id, user.id, invitation_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/group-invitations/preview",
    request_body = TokenBody,
    responses(
        (status = 200, description = "The group the token invites to", body = GroupPreviewOut),
        (status = 400, description = "The token is spent or not ours", body = Detail),
        (status = 404, description = "No invitation carries this token", body = Detail),
        (status = 422, description = "The body has no token", body = Detail),
    )
)]
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

#[utoipa::path(
    post,
    path = "/api/group-invitations/accept",
    request_body = TokenBody,
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The group, joined", body = GroupOut),
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
) -> Result<Json<GroupOut>, ApiError> {
    let group = groups::accept_invitation(&state, &body.token, &user).await?;
    Ok(Json(GroupOut::new(group, GroupRole::Member.as_str())))
}
