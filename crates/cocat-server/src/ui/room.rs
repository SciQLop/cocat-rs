//! The room page: its members and, for the owner, its invitations.

use std::str::FromStr;

use askama::Template;
use axum::Form;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use cocat_core::role::Role;
use serde::Deserialize;
use uuid::Uuid;

use crate::ui::frame::{self, Flash, Frame, RequireUser};
use crate::{AppState, db, invitations, lifecycle, mail};

/// Python's `INVITATION_LIFETIME`: every invitation this page makes lasts two weeks.
const INVITATION_DAYS: u32 = 14;

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
    invitations: Vec<InvitationLine>,
}

#[derive(Template)]
#[template(path = "room.html")]
struct RoomPage {
    frame: Frame,
    name: String,
    role: &'static str,
    rooms_href: String,
    leave: Option<LeaveForm>,
    members: Vec<MemberLine>,
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
) -> Response {
    match build_page(&state, &user, &room_id, flash).await {
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
) -> Result<Option<RoomPage>, lifecycle::LifecycleError> {
    let Some((room, role)) = db::list_user_rooms(&state.pool, user.id)
        .await?
        .into_iter()
        .find(|(room, _)| room.id == room_id)
    else {
        return Ok(None);
    };
    let (notice, share_link) = split_flash(flash);
    let members = members_of(state, &room.id, role).await?;
    let panel = match role {
        Role::Owner => Some(panel(state, user.id, &room.id, share_link).await?),
        _ => None,
    };
    let leave = (role != Role::Owner).then(|| LeaveForm {
        action: state.links.path(&format!("/rooms/{room_id}/leave")),
        confirm: format!("Leave {}?", room.name),
    });
    Ok(Some(RoomPage {
        frame: Frame::new(&state.links, Some(user), "Room").flashing(notice),
        name: room.name,
        role: role.as_str(),
        rooms_href: state.links.path("/rooms"),
        leave,
        members,
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

async fn panel(
    state: &AppState,
    actor: Uuid,
    room_id: &str,
    share_link: Option<String>,
) -> Result<InvitationPanel, lifecycle::LifecycleError> {
    let rows = invitations::list(state, room_id, actor).await?;
    Ok(InvitationPanel {
        action: state.links.path(&format!("/rooms/{room_id}/invitations")),
        share_link,
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

/// A flash may carry a share link on a second line: a redirect has no other way to hand the
/// link back, and the notice is all the page shows above it.
fn split_flash(flash: Option<String>) -> (Option<String>, Option<String>) {
    match flash {
        Some(value) => match value.split_once('\n') {
            Some((notice, link)) => (Some(notice.to_owned()), Some(link.to_owned())),
            None => (Some(value), None),
        },
        None => (None, None),
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
    let email = match member_email(&state, member).await {
        Ok(email) => email,
        Err(status) => return status.into_response(),
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
    let email = match member_email(&state, member).await {
        Ok(email) => email,
        Err(status) => return status.into_response(),
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
    let email = match member_email(&state, member).await {
        Ok(email) => email,
        Err(status) => return status.into_response(),
    };
    let result = lifecycle::transfer_ownership(&state, &room_id, user.id, member).await;
    answer(
        &state,
        result,
        &format!("/rooms/{room_id}"),
        format!("{email} now owns the room"),
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
                share_link(&state, &target, "Share link created", &token)
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
            share_link(&state, &target, "Invitation created", &token)
        }
        Ok(_) => redirect(
            &state,
            &target,
            &format!("Invitation sent to {}", address.0),
        ),
        Err(error) => answer(&state, Err(error), &target, String::new()),
    }
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

fn share_link(state: &AppState, target: &str, notice: &str, token: &str) -> Response {
    let link = state.links.ui(&format!("/invitation#{token}"));
    redirect(state, target, &format!("{notice}\n{link}"))
}

fn answer(
    state: &AppState,
    result: Result<(), lifecycle::LifecycleError>,
    target: &str,
    done: String,
) -> Response {
    let text = match result {
        Ok(()) => done,
        Err(lifecycle::LifecycleError::NotFound(text))
        | Err(lifecycle::LifecycleError::Forbidden(text)) => text,
        Err(lifecycle::LifecycleError::Conflict(text))
        | Err(lifecycle::LifecycleError::Invalid(text)) => text.to_owned(),
        Err(error) => {
            tracing::warn!(%error, "a room action failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    redirect(state, target, &text)
}

fn redirect(state: &AppState, target: &str, text: &str) -> Response {
    let response = Redirect::to(&state.links.path(target)).into_response();
    frame::flash(&state.links, response, text)
}

async fn member_email(state: &AppState, id: Uuid) -> Result<String, StatusCode> {
    match db::user_by_id(&state.pool, id).await {
        Ok(Some(user)) => Ok(user.email),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(%error, "cannot read a member");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

fn not_found(state: &AppState, user: &db::User) -> Response {
    let page = NotFoundPage {
        frame: Frame::new(&state.links, Some(user), "Room not found"),
        rooms_href: state.links.path("/rooms"),
    };
    (StatusCode::NOT_FOUND, frame::page(page)).into_response()
}
