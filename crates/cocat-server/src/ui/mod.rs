//! The server-rendered web UI under `/ui`.

mod account;
mod frame;
mod login;
mod reset;
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
        .route("/rooms", get(rooms::page).post(rooms::create))
        .route("/rooms/{room_id}/rename", post(rooms::rename))
        .route("/rooms/{room_id}/delete", post(rooms::delete))
        .route("/rooms/{room_id}/leave", post(rooms::leave))
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
            frame::clear_flash,
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
