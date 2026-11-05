//! The frame every page shares: the header, the message table, the extractors and the
//! rule that refuses a form post another site started.

use std::collections::HashMap;

use askama::Template;
use axum::extract::{FromRequestParts, OriginalUri, Path, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Redirect, Response};

use crate::db::User;
use crate::links::Links;
use crate::{AppState, auth, db};

const STYLE: &str = include_str!("../../static/style.css");
const FRAGMENT: &str = include_str!("../../static/fragment.js");
const ASSETS: [(&str, &str, &str); 2] = [
    ("style.css", STYLE, "text/css; charset=utf-8"),
    ("fragment.js", FRAGMENT, "text/javascript; charset=utf-8"),
];

/// Python's `ui/auth.py::MESSAGES`, which pages look up by query flag.
const MESSAGES: [(&str, &str); 10] = [
    ("credentials", "Wrong email or password."),
    ("too_many", "Too many attempts. Try again in a minute."),
    ("exists", "That email is already registered."),
    (
        "unverified",
        "Check your inbox: this account is not verified yet.",
    ),
    ("created", "Account created. Check your inbox to verify it."),
    ("reset", "Password changed. Sign in with the new one."),
    (
        "sent",
        "If that address has an account, a reset link is on its way.",
    ),
    (
        "resent",
        "If that account is not verified yet, a new link is on its way.",
    ),
    ("email", "That is not an email address."),
    ("provider", "The provider could not sign you in. Try again."),
];

/// Python's `error_key` rule: the first truthy flag among the four success flags, else
/// `error`; a key the table does not know renders nothing.
pub(crate) fn message(query: &HashMap<String, String>) -> &'static str {
    let key = ["created", "sent", "reset", "resent"]
        .into_iter()
        .find(|flag| query.get(*flag).is_some_and(|value| !value.is_empty()))
        .or_else(|| query.get("error").map(String::as_str))
        .unwrap_or_default();
    MESSAGES
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, text)| *text)
        .unwrap_or_default()
}

/// The signed-in visitor's part of the header, present only when someone is signed in.
pub(crate) struct Header {
    pub email: String,
    pub rooms_href: String,
    pub account_href: String,
    pub logout_action: String,
}

/// What every page passes to `base.html`.
pub(crate) struct Frame {
    pub title: &'static str,
    pub style_href: String,
    pub header: Option<Header>,
}

impl Frame {
    pub(crate) fn new(links: &Links, user: Option<&User>, title: &'static str) -> Self {
        Self {
            title,
            style_href: links.path("/static/style.css"),
            header: user.map(|user| Header {
                email: user.email.clone(),
                rooms_href: links.path("/rooms"),
                account_href: links.path("/account"),
                logout_action: links.path("/logout"),
            }),
        }
    }
}

/// Render a page, or answer 500 when the template cannot be rendered.
pub(crate) fn page(template: impl Template) -> Response {
    match template.render() {
        Ok(body) => Html(body).into_response(),
        Err(error) => {
            tracing::warn!(%error, "cannot render a page");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// The visitor behind the session cookie, however many pages want them.
pub(crate) struct MaybeUser(pub Option<User>);

impl FromRequestParts<AppState> for MaybeUser {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(signed_in_user(state, &parts.headers).await))
    }
}

/// A page that cannot be answered without a user.
pub(crate) struct RequireUser(pub User);

impl FromRequestParts<AppState> for RequireUser {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        match signed_in_user(state, &parts.headers).await {
            Some(user) => Ok(Self(user)),
            None => Err(login_redirect(&state.links, request_path(parts))),
        }
    }
}

/// The path the browser asked for, before the `/ui` nest stripped its prefix.
fn request_path(parts: &Parts) -> &str {
    match parts.extensions.get::<OriginalUri>() {
        Some(OriginalUri(uri)) => uri.path(),
        None => parts.uri.path(),
    }
}

/// A deactivated account reads as anonymous here: `auth::authenticate` refuses it, so a
/// cookie minted before the deactivation cannot keep acting on a page.
async fn signed_in_user(state: &AppState, headers: &HeaderMap) -> Option<User> {
    let id = auth::authenticate(&state.pool, headers, &state.secret)
        .await
        .ok()?;
    match db::user_by_id(&state.pool, id).await {
        Ok(user) => user,
        Err(error) => {
            tracing::warn!(%error, "cannot read the signed-in user");
            None
        }
    }
}

fn login_redirect(links: &Links, path: &str) -> Response {
    let target = format!(
        "{}?next={}",
        links.path("/login"),
        crate::ui::query_value(path)
    );
    Redirect::temporary(&target).into_response()
}

/// Refuse a form post another site started, before any route can act on it.
///
/// The layer sits on the whole server so a UI route cannot be added without it; only a POST
/// under `/ui` is checked. Missing headers pass: that is a test client or an old browser, and
/// the session cookie is `SameSite=Lax` besides. Python's `same_site` compares the Origin's
/// netloc with the Host header and accepts only a same-origin or same-site `Sec-Fetch-Site`.
pub(crate) async fn same_site(request: Request, next: Next) -> Response {
    if is_ui(request.uri().path())
        && request.method() == Method::POST
        && cross_site(request.headers())
    {
        return (StatusCode::FORBIDDEN, "Forbidden").into_response();
    }
    next.run(request).await
}

fn is_ui(path: &str) -> bool {
    path == "/ui" || path.starts_with("/ui/")
}

fn cross_site(headers: &HeaderMap) -> bool {
    let foreign_origin = headers.get(header::ORIGIN).is_some_and(|origin| {
        let host = headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok());
        match origin.to_str() {
            Ok(origin) => Some(origin_host(origin)) != host,
            Err(_) => true,
        }
    });
    let foreign_fetch = headers
        .get("sec-fetch-site")
        .is_some_and(|site| !matches!(site.to_str(), Ok("same-origin") | Ok("same-site")));
    foreign_origin || foreign_fetch
}

/// The netloc of a serialized origin, as `urlsplit(origin).netloc` would give it.
fn origin_host(origin: &str) -> &str {
    match origin.split_once("://") {
        Some((_, rest)) => rest.split(['/', '?', '#']).next().unwrap_or_default(),
        None => "",
    }
}

pub(crate) async fn index(State(state): State<AppState>) -> Response {
    Redirect::temporary(&state.links.path("/rooms")).into_response()
}

#[derive(Template)]
#[template(path = "rooms.html")]
struct RoomsPage {
    frame: Frame,
}

pub(crate) async fn rooms(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
) -> Response {
    page(RoomsPage {
        frame: Frame::new(&state.links, Some(&user), "My rooms"),
    })
}

pub(crate) async fn static_asset(Path(file): Path<String>) -> Response {
    let Some((_, source, content_type)) = ASSETS.iter().find(|(name, _, _)| *name == file) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, *content_type),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        *source,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::message;
    use std::collections::HashMap;

    fn query(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn message_table_matches_python() {
        for (key, text) in [
            ("credentials", "Wrong email or password."),
            ("too_many", "Too many attempts. Try again in a minute."),
            ("exists", "That email is already registered."),
            (
                "unverified",
                "Check your inbox: this account is not verified yet.",
            ),
            ("created", "Account created. Check your inbox to verify it."),
            ("reset", "Password changed. Sign in with the new one."),
            (
                "sent",
                "If that address has an account, a reset link is on its way.",
            ),
            (
                "resent",
                "If that account is not verified yet, a new link is on its way.",
            ),
            ("email", "That is not an email address."),
            ("provider", "The provider could not sign you in. Try again."),
        ] {
            assert_eq!(message(&query(&[("error", key)])), text, "{key}");
        }

        assert_eq!(
            message(&query(&[("created", "1"), ("error", "credentials")])),
            "Account created. Check your inbox to verify it."
        );
        assert_eq!(
            message(&query(&[("sent", "1"), ("reset", "1")])),
            "If that address has an account, a reset link is on its way."
        );
        assert_eq!(message(&query(&[])), "");
        assert_eq!(message(&query(&[("error", "unknown")])), "");
        assert_eq!(
            message(&query(&[("created", ""), ("error", "credentials")])),
            "Wrong email or password."
        );
    }
}
