//! The server-rendered web UI under `/ui`.

mod account;
mod frame;
mod invitation;
mod login;
mod reset;
mod room;
mod rooms;
mod signup;
mod verify;

use axum::Router;
use axum::middleware;
use axum::routing::{get, post};

use crate::AppState;

pub fn router(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/", get(frame::index))
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
        .route("/groups", get(frame::groups))
        .route("/account", get(account::page))
        .route("/account/password", post(account::password))
        .route(
            "/confirm-email",
            get(account::confirm_page).post(account::confirm),
        )
        .route("/static/{file}", get(frame::static_asset))
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
