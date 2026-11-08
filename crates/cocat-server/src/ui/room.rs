//! The old project page, now the app's Share dialog, and the form actions it posted, which
//! old tabs and links may still send.

use std::str::FromStr;

use askama::Template;
use axum::Form;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use cocat_core::role::Role;
use serde::Deserialize;
use uuid::Uuid;

use crate::links::segment;
use crate::ui::frame::{self, Frame, RequireUser};
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

#[derive(Template)]
#[template(path = "room_not_found.html")]
struct NotFoundPage {
    frame: Frame,
    rooms_href: String,
}

/// The project page moved into the app's Share dialog.
pub(crate) async fn page(
    Path(room_id): Path<String>,
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
) -> Response {
    match db::role_in(&state.pool, &room_id, user.id).await {
        Ok(Some(_)) => redirect_to(&state, &format!("{}/browse#share", page_of(&room_id))),
        Ok(None) => not_found(&state, &user),
        Err(error) => {
            tracing::warn!(%error, "cannot read a room page");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
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
        return answer(&state, Err(error), &page_of(&room_id), String::new());
    }
    let email = match member_answer(&state, &room_id, member).await {
        Ok(email) => email,
        Err(response) => return *response,
    };
    let result = lifecycle::set_member_role(&state, &room_id, user.id, member, role).await;
    let done = format!("{email} is now a {}", role.as_str());
    answer(&state, result, &page_of(&room_id), done)
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
        return answer(&state, Err(error), &page_of(&room_id), String::new());
    }
    let email = match member_answer(&state, &room_id, member).await {
        Ok(email) => email,
        Err(response) => return *response,
    };
    let result = lifecycle::remove_member(&state, &room_id, user.id, member).await;
    answer(
        &state,
        result,
        &page_of(&room_id),
        format!("Removed {email}"),
    )
}

pub(crate) async fn transfer(
    Path((room_id, member)): Path<(String, Uuid)>,
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
) -> Response {
    if let Err(error) = lifecycle::require_role(&state, &room_id, user.id, Role::Owner).await {
        return answer(&state, Err(error), &page_of(&room_id), String::new());
    }
    let email = match member_answer(&state, &room_id, member).await {
        Ok(email) => email,
        Err(response) => return *response,
    };
    let result = lifecycle::transfer_ownership(&state, &room_id, user.id, member).await;
    answer(
        &state,
        result,
        &page_of(&room_id),
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
    let target = page_of(&room_id);
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
    let target = page_of(&room_id);
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
    let target = page_of(&room_id);
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
        &page_of(&room_id),
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
        Ok(None) => Err(Box::new(redirect(state, &page_of(room_id), NOT_A_MEMBER))),
        Err(error) => {
            tracing::warn!(%error, "cannot read a member");
            Err(Box::new(StatusCode::INTERNAL_SERVER_ERROR.into_response()))
        }
    }
}

/// The project's old page, which the form actions answer with.
fn page_of(room_id: &str) -> String {
    format!("/rooms/{}", segment(room_id))
}

pub(super) fn not_found(state: &AppState, user: &db::User) -> Response {
    let page = NotFoundPage {
        frame: Frame::new(&state.links, Some(user), "Project not found"),
        rooms_href: state.links.path("/rooms"),
    };
    (StatusCode::NOT_FOUND, frame::page(page)).into_response()
}
