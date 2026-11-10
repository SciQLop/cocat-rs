//! My rooms: the list, and the create, rename, delete and leave actions.

use askama::Template;
use axum::Form;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use cocat_core::role::Role;
use serde::Deserialize;

use crate::links::segment;
use crate::ui::frame::{self, Flash, Frame, RequireUser};
use crate::{AppState, db, lifecycle};

const CREATED: &str = "Project created";
const RENAMED: &str = "Project renamed";
const DELETED: &str = "Project deleted";
const LEFT: &str = "You left the project";

#[derive(Debug, Deserialize)]
pub(crate) struct NameForm {
    #[serde(default)]
    name: String,
}

struct DeleteForm {
    action: String,
    confirm: String,
}

struct LeaveForm {
    action: String,
    confirm: String,
}

/// One room as the list shows it: a link, the role held, and the actions that role allows.
struct RoomLine {
    id: String,
    name: String,
    href: String,
    role: &'static str,
    /// Where a rename goes: owners and editors rename.
    rename: Option<String>,
    delete: Option<DeleteForm>,
    leave: Option<LeaveForm>,
}

#[derive(Template)]
#[template(path = "rooms.html")]
struct RoomsPage {
    frame: Frame,
    action: String,
    rooms: Vec<RoomLine>,
}

/// The list moved into the app's project menu; it stays for someone with no project, whose
/// way into the app is to make one.
pub(crate) async fn page(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Flash(flash): Flash,
) -> Response {
    match super::app::entry(&state, &user).await {
        Ok(Some(target)) => return super::redirect_to(&state, &target),
        Ok(None) => {}
        Err(response) => return *response,
    }
    match db::list_user_rooms(&state.pool, user.id).await {
        Ok(rooms) => frame::page(RoomsPage {
            frame: Frame::new(&state.links, Some(&user), "My projects").flashing(flash),
            action: state.links.path("/rooms"),
            rooms: rooms
                .into_iter()
                .map(|(room, role)| line(&state, room.id, room.name, role))
                .collect(),
        }),
        Err(error) => {
            tracing::warn!(%error, "cannot list the rooms");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn line(state: &AppState, id: String, name: String, role: Role) -> RoomLine {
    let segment = segment(&id);
    let action = |suffix: &str| state.links.path(&format!("/rooms/{segment}/{suffix}"));
    RoomLine {
        href: state.links.path(&format!("/rooms/{segment}")),
        role: role.as_str(),
        rename: role.can_write().then(|| action("rename")),
        delete: (role == Role::Owner).then(|| DeleteForm {
            action: action("delete"),
            confirm: format!("Delete {name}?"),
        }),
        leave: (role != Role::Owner).then(|| LeaveForm {
            action: action("leave"),
            confirm: format!("Leave {name}?"),
        }),
        id,
        name,
    }
}

pub(crate) async fn create(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Form(form): Form<NameForm>,
) -> Response {
    let result = lifecycle::create_room(&state, user.id, &form.name).await;
    answer(&state, result.map(|_| ()), CREATED)
}

pub(crate) async fn rename(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Form(form): Form<NameForm>,
) -> Response {
    let result = lifecycle::rename_room(&state, &room_id, user.id, &form.name).await;
    answer(&state, result.map(|_| ()), RENAMED)
}

pub(crate) async fn delete(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> Response {
    let result = lifecycle::delete_room(&state, &room_id, user.id).await;
    answer(&state, result, DELETED)
}

pub(crate) async fn leave(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> Response {
    let result = lifecycle::remove_member(&state, &room_id, user.id, user.id).await;
    answer(&state, result, LEFT)
}

/// Back to the list with the outcome flashed: the action's own words when the rules refused
/// it, the success message otherwise.
fn answer(state: &AppState, result: Result<(), lifecycle::LifecycleError>, done: &str) -> Response {
    let text = match result {
        Ok(()) => done.to_owned(),
        Err(
            lifecycle::LifecycleError::Invalid(text) | lifecycle::LifecycleError::Conflict(text),
        ) => text.to_owned(),
        Err(
            lifecycle::LifecycleError::Forbidden(text)
            | lifecycle::LifecycleError::NotFound(text)
            | lifecycle::LifecycleError::Relayed(text),
        ) => text,
        Err(error) => {
            tracing::warn!(%error, "a room action failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let redirect = Redirect::to(&state.links.path("/rooms")).into_response();
    frame::flash(&state.links, redirect, &text)
}
