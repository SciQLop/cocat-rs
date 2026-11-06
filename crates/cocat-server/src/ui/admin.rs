//! The administration pages: an overview, the users and the rooms, superusers only.
//!
//! A signed-in caller who is not a superuser gets 404 on the pages, as for a room they
//! cannot see, and the forms refuse with a redirect like every other UI action.

use std::collections::HashMap;

use askama::Template;
use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::admin::{self, AdminError, Overview, RoomRow, UserRow};
use crate::db::User;
use crate::ui::frame::{self, Flash, Frame, RequireUser};

#[derive(Template)]
#[template(path = "admin_overview.html")]
struct OverviewPage {
    frame: Frame,
    tabs: Tabs,
    users: UserTotals,
    rooms: RoomTotals,
    storage: StorageTotals,
    server: ServerView,
    largest: Vec<RoomView>,
    near_quota: Vec<UserView>,
}

#[derive(Template)]
#[template(path = "admin_users.html")]
struct UsersPage {
    frame: Frame,
    tabs: Tabs,
    search_action: String,
    search: String,
    users: Vec<UserView>,
}

#[derive(Template)]
#[template(path = "admin_rooms.html")]
struct RoomsPage {
    frame: Frame,
    tabs: Tabs,
    rooms: Vec<RoomView>,
    room_default: String,
}

/// The three sections, and which one the page shows.
struct Tabs {
    overview: String,
    users: String,
    rooms: String,
    current: &'static str,
}

struct UserTotals {
    total: u64,
    active: u64,
    unverified: u64,
    superusers: u64,
    recent: u64,
}

struct RoomTotals {
    total: u64,
    open: u64,
    clients: u64,
}

struct StorageTotals {
    total: String,
}

struct ServerView {
    version: &'static str,
    uptime: String,
    memory: String,
    room_limit: String,
    owner_limit: String,
}

/// One account as the tables show it, with the forms that act on it.
struct UserView {
    used_bytes: u64,
    email: String,
    joined: String,
    is_active: bool,
    is_verified: bool,
    is_superuser: bool,
    is_self: bool,
    rooms_owned: u64,
    used: String,
    quota: String,
    percent: u64,
    quota_override: String,
    active_action: String,
    verify_action: String,
    superuser_action: String,
    quota_action: String,
    delete_action: String,
    delete_confirm: String,
}

/// One room as the tables show it, with the forms that act on it.
struct RoomView {
    size_bytes: u64,
    id: String,
    name: String,
    owner: String,
    size: String,
    limit: String,
    percent: u64,
    limit_override: String,
    members: u64,
    open: bool,
    clients: u64,
    limit_action: String,
    owner_action: String,
    delete_action: String,
    delete_confirm: String,
}

pub(crate) async fn overview(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Flash(flash): Flash,
) -> Response {
    let overview = match admin::overview(&state, &user).await {
        Ok(overview) => overview,
        Err(error) => return refused_page(error),
    };
    let Overview {
        users,
        rooms,
        storage,
        server,
    } = overview;
    frame::page(OverviewPage {
        frame: frame(&state, &user, "Administration", flash),
        tabs: tabs(&state, "overview"),
        users: UserTotals {
            total: users.total,
            active: users.active,
            unverified: users.unverified,
            superusers: users.superusers,
            recent: users.recent,
        },
        rooms: RoomTotals {
            total: rooms.total,
            open: rooms.open,
            clients: rooms.clients,
        },
        storage: StorageTotals {
            total: bytes(storage.total_bytes),
        },
        server: ServerView {
            version: server.version,
            uptime: duration(server.uptime_secs),
            memory: server
                .resident_bytes
                .map_or_else(|| "unknown".to_owned(), bytes),
            room_limit: bytes(server.room_limit_bytes),
            owner_limit: bytes(server.owner_limit_bytes),
        },
        largest: storage
            .largest
            .into_iter()
            .map(|room| room_view(&state, room))
            .collect(),
        near_quota: storage
            .owners_near_quota
            .into_iter()
            .map(|row| user_view(&state, &user, row))
            .collect(),
    })
}

pub(crate) async fn users(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    Flash(flash): Flash,
) -> Response {
    let search = query
        .get("q")
        .map(|value| value.trim().to_owned())
        .unwrap_or_default();
    let rows = match admin::users(
        &state,
        &user,
        Some(&search).filter(|s| !s.is_empty()).map(String::as_str),
    )
    .await
    {
        Ok(rows) => rows,
        Err(error) => return refused_page(error),
    };
    frame::page(UsersPage {
        frame: frame(&state, &user, "Users", flash),
        tabs: tabs(&state, "users"),
        search_action: state.links.path("/admin/users"),
        users: rows
            .into_iter()
            .map(|row| user_view(&state, &user, row))
            .collect(),
        search,
    })
}

pub(crate) async fn rooms(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Flash(flash): Flash,
) -> Response {
    let rows = match admin::rooms(&state, &user).await {
        Ok(rows) => rows,
        Err(error) => return refused_page(error),
    };
    frame::page(RoomsPage {
        frame: frame(&state, &user, "Rooms", flash),
        tabs: tabs(&state, "rooms"),
        room_default: bytes(state.rooms.limits().room),
        rooms: rows.into_iter().map(|row| room_view(&state, row)).collect(),
    })
}

#[derive(Debug, Deserialize)]
pub(crate) struct FlagForm {
    flag: String,
    value: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct QuotaForm {
    #[serde(default)]
    mb: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OwnerForm {
    #[serde(default)]
    email: String,
}

pub(crate) async fn set_flag(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Path(target): Path<Uuid>,
    Form(form): Form<FlagForm>,
) -> Response {
    if !user.is_superuser {
        return answer(&state, "/admin", Err(AdminError::NotFound), "");
    }
    let value = form.value == "true";
    let (active, verified, superuser) = match form.flag.as_str() {
        "active" => (Some(value), None, None),
        "verified" => (None, Some(value), None),
        "superuser" => (None, None, Some(value)),
        _ => return back(&state, "/admin/users", "Unknown setting"),
    };
    let result = admin::set_user_flags(&state, &user, target, active, verified, superuser).await;
    answer(&state, "/admin/users", result, "Account updated")
}

pub(crate) async fn set_quota(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Path(target): Path<Uuid>,
    Form(form): Form<QuotaForm>,
) -> Response {
    if !user.is_superuser {
        return answer(&state, "/admin", Err(AdminError::NotFound), "");
    }
    let Ok(mb) = megabytes(&form.mb) else {
        return back(&state, "/admin/users", "A quota is a whole number of MB");
    };
    let result = admin::set_user_quota(&state, &user, target, mb).await;
    answer(&state, "/admin/users", result, "Quota saved")
}

pub(crate) async fn delete_user(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Path(target): Path<Uuid>,
) -> Response {
    let result = admin::delete_user(&state, &user, target).await;
    answer(&state, "/admin/users", result, "Account deleted")
}

pub(crate) async fn set_limit(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Path(room): Path<String>,
    Form(form): Form<QuotaForm>,
) -> Response {
    if !user.is_superuser {
        return answer(&state, "/admin", Err(AdminError::NotFound), "");
    }
    let Ok(mb) = megabytes(&form.mb) else {
        return back(&state, "/admin/rooms", "A limit is a whole number of MB");
    };
    let result = admin::set_room_limit(&state, &user, &room, mb).await;
    answer(&state, "/admin/rooms", result, "Limit saved")
}

pub(crate) async fn set_owner(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Path(room): Path<String>,
    Form(form): Form<OwnerForm>,
) -> Response {
    let result = admin::set_room_owner(&state, &user, &room, form.email.trim()).await;
    answer(&state, "/admin/rooms", result, "Owner changed")
}

pub(crate) async fn delete_room(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Path(room): Path<String>,
) -> Response {
    let result = admin::delete_room(&state, &user, &room).await;
    answer(&state, "/admin/rooms", result, "Room deleted")
}

fn frame(state: &AppState, user: &User, title: &'static str, flash: Option<String>) -> Frame {
    Frame::new(&state.links, Some(user), title).flashing(flash)
}

fn tabs(state: &AppState, current: &'static str) -> Tabs {
    Tabs {
        overview: state.links.path("/admin"),
        users: state.links.path("/admin/users"),
        rooms: state.links.path("/admin/rooms"),
        current,
    }
}

fn user_view(state: &AppState, actor: &User, row: UserRow) -> UserView {
    let action = |suffix: &str| {
        state
            .links
            .path(&format!("/admin/users/{}/{suffix}", row.id))
    };
    UserView {
        joined: row.created_at.chars().take(10).collect(),
        is_self: row.id == actor.id,
        used_bytes: row.used_bytes,
        used: bytes(row.used_bytes),
        quota: bytes(row.quota_bytes),
        percent: percent(row.used_bytes, row.quota_bytes),
        quota_override: row
            .quota_override_mb
            .map(|mb| mb.to_string())
            .unwrap_or_default(),
        active_action: action("flag"),
        verify_action: action("flag"),
        superuser_action: action("flag"),
        quota_action: action("quota"),
        delete_action: action("delete"),
        delete_confirm: format!(
            "Delete {}? Their sessions end and the rooms they own lose their owner.",
            row.email
        ),
        email: row.email,
        is_active: row.is_active,
        is_verified: row.is_verified,
        is_superuser: row.is_superuser,
        rooms_owned: row.rooms_owned,
    }
}

fn room_view(state: &AppState, row: RoomRow) -> RoomView {
    let segment = path_segment(&row.id);
    let action = |suffix: &str| {
        state
            .links
            .path(&format!("/admin/rooms/{segment}/{suffix}"))
    };
    RoomView {
        owner: row.owner.clone().unwrap_or_default(),
        size_bytes: row.size_bytes,
        size: bytes(row.size_bytes),
        limit: bytes(row.limit_bytes),
        percent: percent(row.size_bytes, row.limit_bytes),
        limit_override: row
            .limit_override_mb
            .map(|mb| mb.to_string())
            .unwrap_or_default(),
        members: row.members,
        open: row.open,
        clients: row.clients,
        limit_action: action("limit"),
        owner_action: action("owner"),
        delete_action: action("delete"),
        delete_confirm: format!(
            "Delete {}? Its file is removed and everyone in it is disconnected.",
            row.name
        ),
        id: row.id,
        name: row.name,
    }
}

/// An empty field clears the override; anything else must be a whole number of MB.
fn megabytes(text: &str) -> Result<Option<u64>, std::num::ParseIntError> {
    let text = text.trim();
    if text.is_empty() {
        Ok(None)
    } else {
        text.parse().map(Some)
    }
}

/// A room id as one URL path segment: ids imported from Python may hold spaces and dots.
fn path_segment(id: &str) -> String {
    form_urlencoded::byte_serialize(id.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

fn percent(used: u64, limit: u64) -> u64 {
    used.saturating_mul(100)
        .checked_div(limit)
        .map_or(0, |share| share.min(100))
}

fn bytes(count: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = count as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{count} B")
    } else if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

fn duration(seconds: u64) -> String {
    let (days, hours, minutes) = (seconds / 86_400, seconds / 3_600 % 24, seconds / 60 % 60);
    match (days, hours) {
        (0, 0) => format!("{minutes} min"),
        (0, _) => format!("{hours} h {minutes} min"),
        _ => format!("{days} d {hours} h"),
    }
}

fn refused_page(error: AdminError) -> Response {
    match error {
        AdminError::NotFound => StatusCode::NOT_FOUND.into_response(),
        error => {
            tracing::warn!(%error, "an administration page failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Back to the page with the outcome flashed. A caller who is not a superuser is sent back
/// to their rooms, told nothing, as the other forms refuse.
fn answer(state: &AppState, page: &str, result: Result<(), AdminError>, done: &str) -> Response {
    let text = match result {
        Ok(()) => done.to_owned(),
        Err(AdminError::NotFound) => {
            return Redirect::to(&state.links.path("/rooms")).into_response();
        }
        Err(
            error @ (AdminError::OwnAccount | AdminError::InvalidQuota | AdminError::UnknownUser),
        ) => error.to_string(),
        Err(error) => {
            tracing::warn!(%error, "an administration action failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    back(state, page, &text)
}

fn back(state: &AppState, page: &str, text: &str) -> Response {
    let redirect = Redirect::to(&state.links.path(page)).into_response();
    frame::flash(&state.links, redirect, text)
}
