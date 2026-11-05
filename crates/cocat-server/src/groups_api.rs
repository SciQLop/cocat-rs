//! The group routes, kept out of `api` the way `invitations_api` is: the rules live in
//! `groups`, this module only owns the HTTP shape.

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{delete, get, patch};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::api::{ApiError, ApiPath, CurrentUser, JsonBody, Name};
use crate::groups::{self, GroupMember, GroupRole, GroupRow};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/groups", get(list).post(create))
        .route("/groups/{group_id}", patch(rename).delete(remove))
        .route("/groups/{group_id}/members", get(members))
        .route(
            "/groups/{group_id}/members/{user_id}",
            delete(remove_member),
        )
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
    fn new(row: GroupRow, role: GroupRole) -> Self {
        Self {
            id: row.id,
            name: row.name,
            role: role.as_str(),
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

async fn list(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<GroupsOut>, ApiError> {
    let groups = groups::list_for(&state, user.id)
        .await?
        .into_iter()
        .map(|(row, role)| GroupOut::new(row, role))
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
        Json(GroupOut::new(row, GroupRole::Owner)),
    ))
}

async fn rename(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(group_id): ApiPath<String>,
    JsonBody(body): JsonBody<NameBody>,
) -> Result<Json<GroupOut>, ApiError> {
    let row = groups::rename(&state, &group_id, user.id, &body.name.0).await?;
    Ok(Json(GroupOut::new(row, GroupRole::Owner)))
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
