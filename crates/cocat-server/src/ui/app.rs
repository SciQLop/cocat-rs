//! The catalogue browser: its page for one room, and the files of its bundle.
//!
//! The bundle is a trunk build that lives in `COCAT_WEB_DIR`, so the server and the app can
//! be built and shipped apart. The page is the bundle's own `index.html`, with what the app
//! needs to know about the room and the visitor added to its head.

use askama::Template;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use tower_http::services::ServeDir;

use crate::ui::frame::{self, Frame, RequireUser};
use crate::{AppState, db};

const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// What the app reads from its head, all of it escaped by the template.
#[derive(Template)]
#[template(path = "app_head.html")]
struct Head<'a> {
    base: String,
    room_id: &'a str,
    room_name: &'a str,
    role: &'static str,
    socket: String,
    api: String,
    email: &'a str,
}

#[derive(Template)]
#[template(path = "app_missing.html")]
struct MissingPage {
    frame: Frame,
    room_href: String,
}

pub(crate) async fn browse(
    Path(room_id): Path<String>,
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
) -> Response {
    let room = match db::list_user_rooms(&state.pool, user.id).await {
        Ok(rooms) => rooms.into_iter().find(|(room, _)| room.id == room_id),
        Err(error) => {
            tracing::warn!(%error, "cannot read the rooms of a visitor");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let Some((room, role)) = room else {
        return super::room::not_found(&state, &user);
    };
    let Some(index) = index(&state).await else {
        return missing(&state, &user, &room_id);
    };
    let head = Head {
        base: state.links.path("/app/"),
        room_id: &room.id,
        room_name: &room.name,
        role: role.as_str(),
        socket: state.links.socket(&format!("/room/{}", room.id)),
        api: state.links.app("/api"),
        email: &user.email,
    };
    match head.render() {
        Ok(head) => page(&index, &head),
        Err(error) => {
            tracing::warn!(%error, "cannot render the catalogue browser's head");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// The bundle's `index.html`, or `None` when no bundle is installed.
async fn index(state: &AppState) -> Option<String> {
    let dir = state.web_dir.as_ref()?;
    match tokio::fs::read_to_string(dir.join("index.html")).await {
        Ok(index) => Some(index),
        Err(error) => {
            tracing::warn!(%error, dir = %dir.display(), "the catalogue browser's index is unreadable");
            None
        }
    }
}

/// The index with `head` first in its `<head>`: the `<base>` must precede every URL it
/// resolves. The page names the visitor, so no cache keeps it.
fn page(index: &str, head: &str) -> Response {
    let Some(at) = index.find("<head>").map(|at| at + "<head>".len()) else {
        tracing::warn!("the catalogue browser's index has no <head>");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let body = format!("{}\n{head}{}", &index[..at], &index[at..]);
    ([(header::CACHE_CONTROL, "no-store")], Html(body)).into_response()
}

fn missing(state: &AppState, user: &db::User, room_id: &str) -> Response {
    let page = MissingPage {
        frame: Frame::new(&state.links, Some(user), "Catalogues unavailable"),
        room_href: state.links.path(&format!("/rooms/{room_id}")),
    };
    (StatusCode::SERVICE_UNAVAILABLE, frame::page(page)).into_response()
}

/// A file of the bundle. `ServeDir` refuses `..` and absolute paths, and directories are
/// refused rather than listed or completed with their `index.html`.
pub(crate) async fn file(State(state): State<AppState>, mut request: Request) -> Response {
    let Some(dir) = state.web_dir.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(path) = request
        .uri()
        .path()
        .strip_prefix("/app")
        .and_then(|path| Uri::try_from(path).ok())
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    *request.uri_mut() = path;
    let cache = if fingerprinted(request.uri().path()) {
        IMMUTABLE
    } else {
        "no-cache"
    };
    match ServeDir::new(dir)
        .append_index_html_on_directories(false)
        .try_call(request)
        .await
    {
        Ok(response) => {
            let mut response = response.map(Body::new);
            if response.status().is_success() {
                response
                    .headers_mut()
                    .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
            }
            response
        }
        Err(error) => {
            tracing::warn!(%error, "cannot read a file of the catalogue browser");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// trunk names a fingerprinted file `<name>-<16 hex digits>.<ext>` (the wasm gets `_bg`
/// before its extension), so a file so named never changes content.
fn fingerprinted(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.split('.').next().unwrap_or_default();
    let stem = stem.strip_suffix("_bg").unwrap_or(stem);
    stem.rsplit_once('-').is_some_and(|(_, hash)| {
        hash.len() == 16 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

#[cfg(test)]
mod tests {
    use super::fingerprinted;

    #[test]
    fn trunk_hashes_are_recognised() {
        assert!(fingerprinted("/cocat-web-26a4abee7c03bfbb_bg.wasm"));
        assert!(fingerprinted("/cocat-web-af87e39acabebcdf.js"));
        assert!(fingerprinted("/style-15c70349f3048925.css"));
        assert!(!fingerprinted("/mark.png"));
        assert!(!fingerprinted("/index.html"));
        assert!(!fingerprinted(
            "/snippets/cocat-web-af87e39acabebcdf/inline0.js"
        ));
        assert!(!fingerprinted("/cocat-web-notahexhashxxxxx.js"));
    }
}
