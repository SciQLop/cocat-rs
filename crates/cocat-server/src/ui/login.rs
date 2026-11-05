//! The sign-in page, its forms, and the resend that follows a missing confirmation mail.

use std::collections::HashMap;
use std::net::SocketAddr;

use askama::Template;
use axum::extract::{ConnectInfo, Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

use crate::auth::AuthError;
use crate::ui::frame::{self, Frame, MaybeUser};
use crate::ui::next_query;
use crate::{AppState, accounts, auth};

#[derive(Debug, Deserialize)]
pub(crate) struct ResendForm {
    email: String,
}

/// A provider that can sign a visitor in; empty until phase 3c adds OAuth.
struct Provider {
    name: String,
    href: String,
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage {
    frame: Frame,
    action: String,
    message: &'static str,
    sign_up: String,
    forgot: String,
    resend_action: String,
    providers: Vec<Provider>,
}

pub(crate) async fn page(
    MaybeUser(user): MaybeUser,
    Query(query): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> Response {
    if user.is_some() {
        return Redirect::temporary(&state.links.path("/rooms")).into_response();
    }
    frame::page(LoginPage {
        frame: Frame::new(&state.links, None, "Sign in"),
        action: format!(
            "{}{}",
            state.links.path("/login"),
            next_query(query.get("next").map(String::as_str))
        ),
        message: frame::message(&query),
        sign_up: state.links.path("/signup"),
        forgot: state.links.path("/forgot-password"),
        resend_action: state.links.path("/request-verify"),
        providers: Vec::new(),
    })
}

pub(crate) async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Form(form): Form<auth::LoginForm>,
) -> Response {
    if !state.login_limit.hit("/ui/login", &headers, Some(peer)) {
        return failed(&state, "too_many");
    }
    let verified_only = state.mailer.is_some();
    match auth::login(
        &state.pool,
        &state.secret,
        &form.username,
        &form.password,
        verified_only,
    )
    .await
    {
        Ok(token) => {
            let target = safe_next(query.get("next").map(String::as_str), &state.links);
            (
                [(header::SET_COOKIE, auth::set_cookie(&token))],
                Redirect::to(&target),
            )
                .into_response()
        }
        Err(AuthError::BadCredentials | AuthError::InactiveUser) => failed(&state, "credentials"),
        Err(AuthError::NotVerified) => failed(&state, "unverified"),
        Err(error) => {
            tracing::warn!(%error, "login failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub(crate) async fn logout(State(state): State<AppState>) -> Response {
    (
        [(header::SET_COOKIE, auth::clear_cookie())],
        Redirect::to(&state.links.path("/login")),
    )
        .into_response()
}

pub(crate) async fn request_verify(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<ResendForm>,
) -> Response {
    if !state
        .login_limit
        .hit("/ui/request-verify", &headers, Some(peer))
    {
        return failed(&state, "too_many");
    }
    let target = format!("{}?resent=1", state.links.path("/login"));
    tokio::spawn(async move { accounts::request_verification(&state, &form.email).await });
    Redirect::to(&target).into_response()
}

fn failed(state: &AppState, key: &str) -> Response {
    Redirect::to(&format!("{}?error={key}", state.links.path("/login"))).into_response()
}

/// Only follow a `next` that stays on this site; anything else is the rooms page.
///
/// The value is refused when it names a scheme or host, contains a backslash, or starts
/// with a slash pair browsers would collapse into a host. It is also decoded once and
/// checked again, because `/%2F%2Fhost` reaches the server unchanged and a browser would
/// read its decoded form as `//host`.
pub(crate) fn safe_next(value: Option<&str>, links: &crate::links::Links) -> String {
    let default = links.path("/rooms");
    let Some(value) = value else {
        return default;
    };
    if !local_path(value) {
        return default;
    }
    let decoded = percent_decode(value);
    if decoded != value && !local_path(&decoded) {
        return default;
    }
    value.to_owned()
}

fn local_path(value: &str) -> bool {
    value.starts_with('/') && !value.contains('\\') && !value.starts_with("//")
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if let Some(byte) = escape(bytes, index) {
            decoded.push(byte);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn escape(bytes: &[u8], index: usize) -> Option<u8> {
    if bytes.get(index) != Some(&b'%') {
        return None;
    }
    let high = hex(*bytes.get(index + 1)?)?;
    let low = hex(*bytes.get(index + 2)?)?;
    Some(high * 16 + low)
}

fn hex(byte: u8) -> Option<u8> {
    char::from(byte).to_digit(16).map(|digit| digit as u8)
}
