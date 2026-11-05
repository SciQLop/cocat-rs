//! The server-rendered web UI under `/ui`.

mod frame;

use axum::Router;
use axum::routing::get;

use crate::AppState;

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/", get(frame::index))
        .route("/rooms", get(frame::rooms))
        .route("/static/{file}", get(frame::static_asset))
}

pub(crate) use frame::same_site;

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
