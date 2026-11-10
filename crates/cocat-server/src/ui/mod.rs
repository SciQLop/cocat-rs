//! The server-rendered web UI under `/ui`.

mod account;
mod admin;
mod app;
mod connect;
mod frame;
mod groups;
mod help;
mod invitation;
mod landing;
mod login;
mod oauth;
mod reset;
mod room;
mod room_page;
mod rooms;
mod signup;
mod verify;

use axum::Router;
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};

pub(crate) use landing::landing;

use crate::AppState;
use crate::lifecycle::LifecycleError;

pub fn router(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/", get(frame::index))
        .route("/help", get(help::page))
        .route("/login", get(login::page).post(login::login))
        .route("/logout", post(login::logout))
        .route("/request-verify", post(login::request_verify))
        .route("/signup", get(signup::page).post(signup::signup))
        .route(
            "/forgot-password",
            get(reset::forgot_page).post(reset::forgot),
        )
        .route("/reset-password", get(reset::reset_page).post(reset::reset))
        .route("/verify", get(verify::page).post(verify::submit))
        .route(
            "/invitation",
            get(invitation::page).post(invitation::preview),
        )
        .route("/invitation/accept", post(invitation::accept))
        .route("/rooms", get(rooms::page).post(rooms::create))
        .route("/rooms/{room_id}", get(room::page))
        .route("/rooms/{room_id}/browse", get(app::browse))
        .route("/rooms/{room_id}/rename", post(rooms::rename))
        .route("/rooms/{room_id}/delete", post(rooms::delete))
        .route("/rooms/{room_id}/leave", post(rooms::leave))
        .route(
            "/rooms/{room_id}/members/{member}/role",
            post(room::set_role),
        )
        .route(
            "/rooms/{room_id}/members/{member}/remove",
            post(room::remove),
        )
        .route(
            "/rooms/{room_id}/editors-can-share",
            post(room::editors_can_share),
        )
        .route(
            "/rooms/{room_id}/members/{member}/owner",
            post(room::transfer),
        )
        .route("/rooms/{room_id}/invitations", post(room::invite))
        .route("/rooms/{room_id}/groups", post(room::share_group))
        .route(
            "/rooms/{room_id}/groups/{group}/remove",
            post(room::stop_sharing_group),
        )
        .route(
            "/rooms/{room_id}/invitations/{invitation}/revoke",
            post(room::revoke),
        )
        .route("/groups", get(groups::page).post(groups::create))
        .route(
            "/groups/join",
            get(groups::join_page).post(groups::join_preview),
        )
        .route("/groups/join/accept", post(groups::join_accept))
        .route("/groups/{group_id}", get(groups::detail))
        .route("/groups/{group_id}/rename", post(groups::rename))
        .route("/groups/{group_id}/delete", post(groups::delete))
        .route("/groups/{group_id}/leave", post(groups::leave))
        .route(
            "/groups/{group_id}/members/{member}/remove",
            post(groups::remove_member),
        )
        .route("/groups/{group_id}/invitations", post(groups::invite))
        .route(
            "/groups/{group_id}/invitations/{invitation}/revoke",
            post(groups::revoke),
        )
        .route("/admin", get(admin::overview))
        .route("/admin/users", get(admin::users))
        .route("/admin/users/{user}/flag", post(admin::set_flag))
        .route("/admin/users/{user}/quota", post(admin::set_quota))
        .route("/admin/users/{user}/delete", post(admin::delete_user))
        .route("/admin/rooms", get(admin::rooms))
        .route("/admin/rooms/{room}/limit", post(admin::set_limit))
        .route("/admin/rooms/{room}/owner", post(admin::set_owner))
        .route("/admin/rooms/{room}/delete", post(admin::delete_room))
        .route("/account", get(account::page))
        .route("/account/password", post(account::password))
        .route("/connect", get(connect::page).post(connect::decide))
        .route("/account/tokens/{id}/revoke", post(account::revoke_app))
        .route(
            "/confirm-email",
            get(account::confirm_page).post(account::confirm),
        )
        .route("/oauth/{name}/login", get(oauth::login))
        .route("/oauth/{name}/callback", get(oauth::callback))
        .route("/static/{file}", get(frame::static_asset))
        .route("/app", get(app::open))
        .route("/app/{*path}", get(app::file))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            frame::clear_notices,
        ))
}

pub(crate) use frame::same_site;

/// The `?next=` a form action carries, re-encoded from the value the page received.
pub(crate) fn next_query(value: Option<&str>) -> String {
    value
        .map(|value| format!("?next={}", query_value(value)))
        .unwrap_or_default()
}

/// What `urlencode` writes for a query value: `application/x-www-form-urlencoded`, so a
/// path keeps its `/` percent-encoded and a space becomes `+`.
pub(crate) fn query_value(value: &str) -> String {
    form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// Python's `token_from_text`: what follows the `#`, else the paste itself, first word.
pub(crate) fn token_from_text(text: &str) -> String {
    let after_hash = text.split_once('#').map_or(text, |(_, token)| token);
    after_hash
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// Turn a rule-module outcome into the redirect the page expects, flashing either the
/// caller's success text or the error detail the module gave.
pub(crate) fn answer(
    state: &AppState,
    result: Result<(), LifecycleError>,
    target: &str,
    done: String,
) -> Response {
    let text = match result {
        Ok(()) => done,
        Err(LifecycleError::NotFound(text))
        | Err(LifecycleError::Forbidden(text))
        | Err(LifecycleError::Relayed(text)) => text,
        Err(LifecycleError::Conflict(text)) | Err(LifecycleError::Invalid(text)) => text.to_owned(),
        Err(error) => {
            tracing::warn!(%error, "a page action failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    redirect(state, target, &text)
}

pub(crate) fn redirect(state: &AppState, target: &str, text: &str) -> Response {
    frame::flash(&state.links, redirect_to(state, target), text)
}

pub(crate) fn redirect_to(state: &AppState, target: &str) -> Response {
    Redirect::to(&state.links.path(target)).into_response()
}

#[cfg(test)]
mod tests {
    use super::query_value;

    #[test]
    fn query_value_encodes_like_a_form() {
        assert_eq!(query_value("/ui/rooms"), "%2Fui%2Frooms");
        assert_eq!(query_value("/ui/a b?c=d&e"), "%2Fui%2Fa+b%3Fc%3Dd%26e");
        assert_eq!(query_value("é"), "%C3%A9");
        assert_eq!(query_value("~"), "%7E");
        assert_eq!(query_value("*"), "*");
    }
}
