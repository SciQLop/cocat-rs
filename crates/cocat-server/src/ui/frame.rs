//! The frame every page shares: the header, the message table, the extractors and the
//! rule that refuses a form post another site started.

use std::collections::HashMap;
use std::convert::Infallible;

use askama::Template;
use axum::extract::{FromRequestParts, OriginalUri, Path, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Redirect, Response};
use cookie::{Cookie, SameSite};
use url::Url;

use crate::db::User;
use crate::links::Links;
use crate::{AppState, auth, db};

const STYLE: &str = include_str!("../../static/style.css");
const FRAGMENT: &str = include_str!("../../static/fragment.js");
const CONFIRM: &str = include_str!("../../static/confirm.js");
const COPY: &str = include_str!("../../static/copy.js");
const ASSETS: [(&str, &str, &str); 4] = [
    ("style.css", STYLE, "text/css; charset=utf-8"),
    ("fragment.js", FRAGMENT, "text/javascript; charset=utf-8"),
    ("confirm.js", CONFIRM, "text/javascript; charset=utf-8"),
    ("copy.js", COPY, "text/javascript; charset=utf-8"),
];

/// Python's `ui/auth.py::MESSAGES`, which pages look up by query flag. `password_changed` is
/// ours: ending the session on a password change is the one behaviour Python does not have.
const MESSAGES: [(&str, &str); 11] = [
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
    (
        "password_changed",
        "Password changed, please sign in again.",
    ),
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
    pub groups_href: String,
    pub account_href: String,
    pub logout_action: String,
}

/// What every page passes to `base.html`.
pub(crate) struct Frame {
    pub title: &'static str,
    pub style_href: String,
    pub scripts: Vec<String>,
    pub header: Option<Header>,
    pub flash: Option<String>,
}

impl Frame {
    pub(crate) fn new(links: &Links, user: Option<&User>, title: &'static str) -> Self {
        Self {
            title,
            style_href: links.path("/static/style.css"),
            scripts: ["fragment.js", "confirm.js", "copy.js"]
                .into_iter()
                .map(|script| links.path(&format!("/static/{script}")))
                .collect(),
            header: user.map(|user| Header {
                email: user.email.clone(),
                rooms_href: links.path("/rooms"),
                groups_href: links.path("/groups"),
                account_href: links.path("/account"),
                logout_action: links.path("/logout"),
            }),
            flash: None,
        }
    }

    /// Carry the one-shot message the page shows and then clears.
    pub(crate) fn flashing(mut self, flash: Option<String>) -> Self {
        self.flash = flash;
        self
    }
}

/// The one-shot message cookie: a rule action answers a 303 and the page it lands on shows
/// the message once, then clears the cookie.
const FLASH: &str = "cocat_flash";
/// The share link a create action hands back through its redirect, in a cookie of its own so
/// no message text can ever fill the field it lands in.
const SHARE_LINK: &str = "cocat_share_link";
const NOTICES: [&str; 2] = [FLASH, SHARE_LINK];

/// Attach `text` to the redirect, for the page the browser asks for next.
pub(crate) fn flash(links: &Links, response: Response, text: &str) -> Response {
    with_notice(
        links,
        response,
        FLASH,
        text,
        Some(cookie::time::Duration::minutes(1)),
    )
}

/// Attach the share link the next page shows, beside (not instead of) its notice.
pub(crate) fn share_link(links: &Links, response: Response, link: &str) -> Response {
    with_notice(
        links,
        response,
        SHARE_LINK,
        link,
        Some(cookie::time::Duration::minutes(1)),
    )
}

fn with_notice(
    links: &Links,
    response: Response,
    name: &str,
    text: &str,
    max_age: Option<cookie::time::Duration>,
) -> Response {
    with_cookie(response, &notice_cookie(links, name, text, max_age))
}

/// The message the request carries, if any.
pub(crate) struct Flash(pub Option<String>);

impl FromRequestParts<AppState> for Flash {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(cookie_value(&parts.headers, FLASH)))
    }
}

/// The share link the request carries, if any.
pub(crate) struct ShareLink(pub Option<String>);

impl FromRequestParts<AppState> for ShareLink {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(cookie_value(&parts.headers, SHARE_LINK)))
    }
}

/// Clear a notice cookie on the answer to the request that showed it. A request arriving
/// without the cookie — the POST that set it, or a second visit — is left alone.
pub(crate) async fn clear_notices(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let carried: Vec<&str> = NOTICES
        .into_iter()
        .filter(|name| cookie_value(request.headers(), name).is_some())
        .collect();
    let response = next.run(request).await;
    carried.into_iter().fold(response, |response, name| {
        with_notice(
            &state.links,
            response,
            name,
            "",
            Some(cookie::time::Duration::seconds(0)),
        )
    })
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    Cookie::split_parse_encoded(cookies)
        .filter_map(Result::ok)
        .find(|cookie| cookie.name() == name)
        .map(|cookie| cookie.value().to_owned())
}

fn notice_cookie(
    links: &Links,
    name: &str,
    text: &str,
    max_age: Option<cookie::time::Duration>,
) -> String {
    let mut builder = Cookie::build((name.to_owned(), text.to_owned()))
        .path(links.path(""))
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Lax);
    if let Some(age) = max_age {
        builder = builder.max_age(age);
    }
    builder.build().encoded().to_string()
}

fn with_cookie(mut response: Response, cookie: &str) -> Response {
    match HeaderValue::from_str(cookie) {
        Ok(value) => {
            response.headers_mut().append(header::SET_COOKIE, value);
            response
        }
        Err(error) => {
            tracing::warn!(%error, "cannot write a response cookie");
            response
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
    let id = auth::authenticate(&state.pool, headers).await.ok()?;
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
            Ok(origin) => origin_authority(origin).as_deref() != host,
            Err(_) => true,
        }
    });
    let foreign_fetch = headers
        .get("sec-fetch-site")
        .is_some_and(|site| !matches!(site.to_str(), Ok("same-origin") | Ok("same-site")));
    foreign_origin || foreign_fetch
}

/// The `host[:port]` an Origin names, compared with the Host header as Python's
/// `urlsplit(origin).netloc` does. An opaque (`null`) or unparsable origin names none.
fn origin_authority(origin: &str) -> Option<String> {
    let url = Url::parse(origin).ok()?;
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

pub(crate) async fn index(State(state): State<AppState>) -> Response {
    Redirect::temporary(&state.links.path("/rooms")).into_response()
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
    use super::{cross_site, message};
    use axum::http::{HeaderMap, header};
    use std::collections::HashMap;

    fn headers(origin: &str, host: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, origin.parse().unwrap());
        headers.insert(header::HOST, host.parse().unwrap());
        headers
    }

    #[test]
    fn a_same_origin_post_is_not_cross_site() {
        assert!(!cross_site(&headers(
            "https://cocat.example.org",
            "cocat.example.org"
        )));
        assert!(!cross_site(&headers(
            "http://localhost:8080",
            "localhost:8080"
        )));
    }

    #[test]
    fn another_host_or_port_is_cross_site() {
        assert!(cross_site(&headers(
            "https://evil.example.org",
            "cocat.example.org"
        )));
        assert!(cross_site(&headers(
            "https://cocat.example.org:8443",
            "cocat.example.org"
        )));
    }

    #[test]
    fn an_opaque_or_unparsable_origin_is_cross_site() {
        assert!(cross_site(&headers("null", "cocat.example.org")));
        assert!(cross_site(&headers(
            "cocat.example.org",
            "cocat.example.org"
        )));
    }

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
