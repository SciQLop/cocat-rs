//! The sign-in page, its forms, and the resend that follows a missing confirmation mail.

use std::collections::HashMap;
use std::net::SocketAddr;

use askama::Template;
use axum::extract::{ConnectInfo, Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use url::Url;

use crate::auth::AuthError;
use crate::ui::frame::{self, Frame, MaybeUser};
use crate::ui::next_query;
use crate::{AppState, accounts, auth};

const LOCAL_ORIGIN: &str = "http://cocat.invalid/";
const LOCAL_HOST: &str = "cocat.invalid";

#[derive(Debug, Deserialize)]
pub(crate) struct ResendForm {
    email: String,
}

/// A provider that can sign a visitor in.
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
        providers: state
            .oidc
            .iter()
            .map(|provider| Provider {
                name: provider.config.name.clone(),
                href: state
                    .links
                    .path(&format!("/oauth/{}/login", provider.config.name)),
            })
            .collect(),
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
/// The value is resolved against a placeholder origin the way a browser resolves a
/// `Location`, so tab, carriage return and newline are stripped and a backslash counts
/// as a slash before the host is compared. Only the path, query and fragment of the
/// resolved URL are returned, so no unparsed input reaches the `Location` header.
pub(crate) fn safe_next(value: Option<&str>, links: &crate::links::Links) -> String {
    let default = links.path("/rooms");
    let Some(value) = value else {
        return default;
    };
    if !value.starts_with('/') {
        return default;
    }
    let Ok(base) = Url::parse(LOCAL_ORIGIN) else {
        return default;
    };
    let Ok(joined) = base.join(value) else {
        return default;
    };
    let local = joined.scheme() == "http"
        && joined.host_str() == Some(LOCAL_HOST)
        && joined.port().is_none()
        && joined.username().is_empty()
        && joined.password().is_none();
    if !local {
        return default;
    }
    let mut target = joined.path().to_owned();
    if let Some(query) = joined.query() {
        target.push('?');
        target.push_str(query);
    }
    if let Some(fragment) = joined.fragment() {
        target.push('#');
        target.push_str(fragment);
    }
    target
}
