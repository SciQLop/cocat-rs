//! The room page: its members and, for the owner, its invitations.

use std::str::FromStr;

use askama::Template;
use axum::Form;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use cocat_core::role::Role;
use serde::Deserialize;
use uuid::Uuid;

use crate::ui::frame::{self, Flash, Frame, RequireUser, ShareLink};
use crate::ui::{answer, redirect, redirect_to};
use crate::{AppState, db, groups, invitations, lifecycle, mail};

/// Python's `INVITATION_LIFETIME`: every invitation this page makes lasts two weeks.
const INVITATION_DAYS: u32 = 14;

/// A caller who may act gets this for an id that is not a member, known or not.
const NOT_A_MEMBER: &str = "That user is not a member here.";

#[derive(Debug, Deserialize)]
pub(crate) struct RoleForm {
    #[serde(default)]
    role: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct InviteForm {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    email: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GroupGrantForm {
    #[serde(default)]
    group: String,
    #[serde(default)]
    role: String,
}

struct LeaveForm {
    action: String,
    confirm: String,
}

struct MemberControls {
    role_action: String,
    remove_action: String,
    owner_action: String,
    remove_confirm: String,
    transfer_confirm: String,
}

struct MemberLine {
    id: String,
    email: String,
    role: &'static str,
    controls: Option<MemberControls>,
}

struct InvitationLine {
    id: i64,
    label: String,
    role: &'static str,
    expires: String,
    usage: String,
    action: String,
}

struct InvitationPanel {
    action: String,
    share_link: Option<String>,
    suggestions: Vec<String>,
    invitations: Vec<InvitationLine>,
}

struct GroupOption {
    value: String,
    label: String,
}

struct GroupControls {
    action: String,
    remove_action: String,
    confirm: String,
}

struct GroupLine {
    id: String,
    name: String,
    role: &'static str,
    controls: Option<GroupControls>,
}

struct GroupPanel {
    action: String,
    picker: Vec<GroupOption>,
    groups: Vec<GroupLine>,
}

#[derive(Template)]
#[template(path = "room.html")]
struct RoomPage {
    frame: Frame,
    name: String,
    role: &'static str,
    rooms_href: String,
    browse_href: String,
    leave: Option<LeaveForm>,
    members: Vec<MemberLine>,
    groups: GroupPanel,
    panel: Option<InvitationPanel>,
}

#[derive(Template)]
#[template(path = "room_not_found.html")]
struct NotFoundPage {
    frame: Frame,
    rooms_href: String,
}

pub(crate) async fn page(
    Path(room_id): Path<String>,
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Flash(flash): Flash,
    ShareLink(link): ShareLink,
) -> Response {
    match build_page(&state, &user, &room_id, flash, link).await {
        Ok(Some(page)) => frame::page(page),
        Ok(None) => not_found(&state, &user),
        Err(error) => {
            tracing::warn!(%error, "cannot read a room page");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn build_page(
    state: &AppState,
    user: &db::User,
    room_id: &str,
    flash: Option<String>,
    share_link: Option<String>,
) -> Result<Option<RoomPage>, lifecycle::LifecycleError> {
    let Some((room, role)) = db::list_user_rooms(&state.pool, user.id)
        .await?
        .into_iter()
        .find(|(room, _)| room.id == room_id)
    else {
        return Ok(None);
    };
    let members = members_of(state, &room.id, role).await?;
    let groups = groups_of(state, user.id, &room.id, role).await?;
    let panel = match role {
        Role::Owner => {
            let emails: Vec<&str> = members.iter().map(|member| member.email.as_str()).collect();
            let suggestions = super::invite_suggestions(state, user.id, &emails).await?;
            Some(panel(state, user.id, &room.id, share_link, suggestions).await?)
        }
        _ => None,
    };
    let leave = (role != Role::Owner).then(|| LeaveForm {
        action: state.links.path(&format!("/rooms/{room_id}/leave")),
        confirm: format!("Leave {}?", room.name),
    });
    Ok(Some(RoomPage {
        frame: Frame::new(&state.links, Some(user), "Project").flashing(flash),
        name: room.name,
        role: role.as_str(),
        rooms_href: state.links.path("/rooms"),
        browse_href: state.links.path(&format!("/rooms/{room_id}/browse")),
        leave,
        members,
        groups,
        panel,
    }))
}

async fn members_of(
    state: &AppState,
    room_id: &str,
    actor_role: Role,
) -> Result<Vec<MemberLine>, lifecycle::LifecycleError> {
    let rows = db::list_members(&state.pool, room_id).await?;
    Ok(rows
        .into_iter()
        .map(|(user, role)| {
            let editable = actor_role == Role::Owner && role != Role::Owner;
            let controls = editable.then(|| MemberControls {
                role_action: state
                    .links
                    .path(&format!("/rooms/{room_id}/members/{}/role", user.id)),
                remove_action: state
                    .links
                    .path(&format!("/rooms/{room_id}/members/{}/remove", user.id)),
                owner_action: state
                    .links
                    .path(&format!("/rooms/{room_id}/members/{}/owner", user.id)),
                remove_confirm: format!("Remove {}?", user.email),
                transfer_confirm: format!("Make {} the owner?", user.email),
            });
            MemberLine {
                id: user.id.to_string(),
                email: user.email,
                role: role.as_str(),
                controls,
            }
        })
        .collect())
}

async fn groups_of(
    state: &AppState,
    actor: Uuid,
    room_id: &str,
    actor_role: Role,
) -> Result<GroupPanel, lifecycle::LifecycleError> {
    let grants = groups::room_groups(state, room_id, actor).await?;
    let granted: Vec<&str> = grants.iter().map(|(group, _)| group.id.as_str()).collect();
    let picker = match actor_role {
        Role::Owner => groups::list_for(state, actor)
            .await?
            .into_iter()
            .filter(|(group, _)| !granted.contains(&group.id.as_str()))
            .map(|(group, _)| GroupOption {
                value: group.id,
                label: group.name,
            })
            .collect(),
        _ => Vec::new(),
    };
    Ok(GroupPanel {
        action: state.links.path(&format!("/rooms/{room_id}/groups")),
        picker,
        groups: grants
            .into_iter()
            .map(|(group, role)| group_line(state, room_id, group, role, actor_role))
            .collect(),
    })
}

fn group_line(
    state: &AppState,
    room_id: &str,
    group: groups::GroupRow,
    role: Role,
    actor_role: Role,
) -> GroupLine {
    let controls = (actor_role == Role::Owner).then(|| GroupControls {
        action: state.links.path(&format!("/rooms/{room_id}/groups")),
        remove_action: state
            .links
            .path(&format!("/rooms/{room_id}/groups/{}/remove", group.id)),
        confirm: format!("Stop sharing with {}?", group.name),
    });
    GroupLine {
        id: group.id,
        name: group.name,
        role: role.as_str(),
        controls,
    }
}

async fn panel(
    state: &AppState,
    actor: Uuid,
    room_id: &str,
    share_link: Option<String>,
    suggestions: Vec<String>,
) -> Result<InvitationPanel, lifecycle::LifecycleError> {
    let rows = invitations::list(state, room_id, actor).await?;
    Ok(InvitationPanel {
        action: state.links.path(&format!("/rooms/{room_id}/invitations")),
        share_link,
        suggestions,
        invitations: rows
            .into_iter()
            .map(|invitation| InvitationLine {
                label: invitation
                    .email
                    .clone()
                    .unwrap_or_else(|| "Share link".to_owned()),
                role: invitation.role.as_str(),
                expires: invitation
                    .expires_at
                    .get(..10)
                    .unwrap_or(&invitation.expires_at)
                    .to_owned(),
                usage: usage(&invitation),
                action: state.links.path(&format!(
                    "/rooms/{room_id}/invitations/{}/revoke",
                    invitation.id
                )),
                id: invitation.id,
            })
            .collect(),
    })
}

fn usage(invitation: &invitations::Invitation) -> String {
    match invitation.max_uses {
        Some(max_uses) => format!("{}/{max_uses}", invitation.uses),
        None => String::new(),
    }
}

pub(crate) async fn set_role(
    Path((room_id, member)): Path<(String, Uuid)>,
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Form(form): Form<RoleForm>,
) -> Response {
    let Ok(role) = Role::from_str(&form.role) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if role == Role::Owner {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if let Err(error) = lifecycle::require_role(&state, &room_id, user.id, Role::Owner).await {
        return answer(
            &state,
            Err(error),
            &format!("/rooms/{room_id}"),
            String::new(),
        );
    }
    let email = match member_answer(&state, &room_id, member).await {
        Ok(email) => email,
        Err(response) => return *response,
    };
    let result = lifecycle::set_member_role(&state, &room_id, user.id, member, role).await;
    let done = format!("{email} is now a {}", role.as_str());
    answer(&state, result, &format!("/rooms/{room_id}"), done)
}

pub(crate) async fn remove(
    Path((room_id, member)): Path<(String, Uuid)>,
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
) -> Response {
    let minimum = if member == user.id {
        Role::Viewer
    } else {
        Role::Owner
    };
    if let Err(error) = lifecycle::require_role(&state, &room_id, user.id, minimum).await {
        return answer(
            &state,
            Err(error),
            &format!("/rooms/{room_id}"),
            String::new(),
        );
    }
    let email = match member_answer(&state, &room_id, member).await {
        Ok(email) => email,
        Err(response) => return *response,
    };
    let result = lifecycle::remove_member(&state, &room_id, user.id, member).await;
    answer(
        &state,
        result,
        &format!("/rooms/{room_id}"),
        format!("Removed {email}"),
    )
}

pub(crate) async fn transfer(
    Path((room_id, member)): Path<(String, Uuid)>,
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
) -> Response {
    if let Err(error) = lifecycle::require_role(&state, &room_id, user.id, Role::Owner).await {
        return answer(
            &state,
            Err(error),
            &format!("/rooms/{room_id}"),
            String::new(),
        );
    }
    let email = match member_answer(&state, &room_id, member).await {
        Ok(email) => email,
        Err(response) => return *response,
    };
    let result = lifecycle::transfer_ownership(&state, &room_id, user.id, member).await;
    answer(
        &state,
        result,
        &format!("/rooms/{room_id}"),
        format!("{email} now owns the project"),
    )
}

pub(crate) async fn invite(
    Path(room_id): Path<String>,
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Form(form): Form<InviteForm>,
) -> Response {
    let Ok(role) = Role::from_str(&form.role) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if role == Role::Owner {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let target = format!("/rooms/{room_id}");
    let address = form.email.trim();
    if form.kind == "link" || address.is_empty() {
        return match invitations::create(
            &state,
            &room_id,
            user.id,
            role,
            None,
            INVITATION_DAYS,
            None,
        )
        .await
        {
            Ok((_, token)) if !token.is_empty() => {
                opened_link(&state, &target, "Share link created", &token)
            }
            Ok(_) => answer(&state, Ok(()), &target, "Share link created".to_owned()),
            Err(error) => answer(&state, Err(error), &target, String::new()),
        };
    }
    let Ok(address) = mail::Address::try_from(address.to_owned()) else {
        return redirect(&state, &target, "That is not an email address.");
    };
    match invitations::create(
        &state,
        &room_id,
        user.id,
        role,
        Some(&address.0),
        INVITATION_DAYS,
        None,
    )
    .await
    {
        Ok((_, token)) if !token.is_empty() => {
            opened_link(&state, &target, "Invitation created", &token)
        }
        Ok(_) => redirect(
            &state,
            &target,
            &format!("Invitation sent to {}", address.0),
        ),
        Err(error) => answer(&state, Err(error), &target, String::new()),
    }
}

pub(crate) async fn share_group(
    Path(room_id): Path<String>,
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Form(form): Form<GroupGrantForm>,
) -> Response {
    let Ok(role) = Role::from_str(&form.role) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if role == Role::Owner {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let target = format!("/rooms/{room_id}");
    let granted = match groups::room_groups(&state, &room_id, user.id).await {
        Ok(grants) => grants.iter().any(|(group, _)| group.id == form.group),
        Err(error) => return answer(&state, Err(error), &target, String::new()),
    };
    match groups::grant(&state, &room_id, &form.group, user.id, role).await {
        Ok((group, _)) if granted => redirect(
            &state,
            &target,
            &format!("{} is now {}", group.name, role.as_str()),
        ),
        Ok((group, _)) => redirect(&state, &target, &format!("Shared with {}", group.name)),
        Err(error) => answer(&state, Err(error), &target, String::new()),
    }
}

pub(crate) async fn stop_sharing_group(
    Path((room_id, group)): Path<(String, String)>,
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
) -> Response {
    let target = format!("/rooms/{room_id}");
    let name = match groups::room_groups(&state, &room_id, user.id).await {
        Ok(grants) => grants
            .into_iter()
            .find(|(granted, _)| granted.id == group)
            .map(|(granted, _)| granted.name),
        Err(error) => return answer(&state, Err(error), &target, String::new()),
    };
    let result = groups::ungrant(&state, &room_id, &group, user.id).await;
    let done = name.map_or_else(
        || "Stopped sharing".to_owned(),
        |name| format!("Stopped sharing with {name}"),
    );
    answer(&state, result, &target, done)
}

pub(crate) async fn revoke(
    Path((room_id, invitation)): Path<(String, i64)>,
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
) -> Response {
    let result = invitations::revoke(&state, &room_id, user.id, invitation).await;
    answer(
        &state,
        result,
        &format!("/rooms/{room_id}"),
        "Invitation revoked".to_owned(),
    )
}

fn opened_link(state: &AppState, target: &str, notice: &str, token: &str) -> Response {
    let link = state.links.ui(&format!("/invitation#{token}"));
    let response = frame::flash(&state.links, redirect_to(state, target), notice);
    frame::share_link(&state.links, response, &link)
}

async fn member_answer(
    state: &AppState,
    room_id: &str,
    member: Uuid,
) -> Result<String, Box<Response>> {
    match db::member_email(&state.pool, room_id, member).await {
        Ok(Some(email)) => Ok(email),
        Ok(None) => Err(Box::new(redirect(
            state,
            &format!("/rooms/{room_id}"),
            NOT_A_MEMBER,
        ))),
        Err(error) => {
            tracing::warn!(%error, "cannot read a member");
            Err(Box::new(StatusCode::INTERNAL_SERVER_ERROR.into_response()))
        }
    }
}

pub(super) fn not_found(state: &AppState, user: &db::User) -> Response {
    let page = NotFoundPage {
        frame: Frame::new(&state.links, Some(user), "Project not found"),
        rooms_href: state.links.path("/rooms"),
    };
    (StatusCode::NOT_FOUND, frame::page(page)).into_response()
}
