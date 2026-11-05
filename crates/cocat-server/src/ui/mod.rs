//! The server-rendered web UI under `/ui`.

mod frame;
mod login;
mod reset;
mod signup;
mod verify;

use axum::Router;
use axum::routing::{get, post};

use crate::AppState;

pub fn router(_state: &AppState) -> Router<AppState> {
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
        .route("/rooms", get(frame::rooms))
        .route("/static/{file}", get(frame::static_asset))
}

pub(crate) use frame::same_site;

/// The `?next=` a form action carries, re-encoded from the value the page received.
pub(crate) fn next_query(value: Option<&str>) -> String {
    value
        .map(|value| format!("?next={}", query_value(value)))
        .unwrap_or_default()
}

/// What Python's `urlencode` writes for a query value: `quote_plus` with no safe character,
/// so a path keeps its `/` percent-encoded and a space becomes `+`.
pub(crate) fn query_value(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            b' ' => encoded.push('+'),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}
