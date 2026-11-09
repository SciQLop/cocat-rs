//! The sign-up page and the form route that creates an account.

use std::collections::HashMap;
use std::net::SocketAddr;

use askama::Template;
use axum::extract::{ConnectInfo, Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

use crate::ui::frame::{self, Frame};
use crate::{AppState, accounts, auth, mail};

#[derive(Debug, Deserialize)]
pub(crate) struct SignupForm {
    email: String,
    password: String,
}

#[derive(Template)]
#[template(path = "signup.html")]
struct SignupPage {
    frame: Frame,
    action: String,
    login_url: String,
    message: String,
}

pub(crate) async fn page(
    Query(query): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> Response {
    signup_page(&state, frame::message(&query))
}

fn signup_page(state: &AppState, message: &str) -> Response {
    frame::page(SignupPage {
        frame: Frame::new(&state.links, None, "Sign up"),
        action: state.links.path("/signup"),
        login_url: state.links.path("/login"),
        message: message.to_owned(),
    })
}

pub(crate) async fn signup(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<SignupForm>,
) -> Response {
    if !state.login_limit.hit("/ui/signup", &headers, Some(peer)) {
        return failed(&state, "too_many");
    }
    let Ok(address) = mail::Address::try_from(form.email.trim().to_owned()) else {
        return failed(&state, "email");
    };
    if mail::is_placeholder(&address.0) {
        return failed(&state, "email");
    }
    let asks_verification = state.mailer.is_some();
    match accounts::register(&state, &address.0, &form.password).await {
        Ok(_) if asks_verification => created(&state),
        Ok(id) => signed_in(&state, id).await,
        Err(accounts::AccountError::Duplicate) if asks_verification => {
            let background = state.clone();
            let email = address.0;
            tokio::spawn(async move { accounts::notify_signup_attempt(&background, &email).await });
            created(&state)
        }
        Err(accounts::AccountError::Duplicate) => failed(&state, "exists"),
        Err(accounts::AccountError::WeakPassword(refusal)) => signup_page(&state, &refusal.0),
        Err(error) => {
            tracing::warn!(%error, "cannot register an account");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// The account still has to be confirmed, so the reply is the same one a fresh address gets:
/// the difference would tell a stranger which addresses are registered.
fn created(state: &AppState) -> Response {
    Redirect::to(&format!("{}?created=1", state.links.path("/login"))).into_response()
}

/// Without a mailer registration verified the account on the spot, so the visitor is in.
async fn signed_in(state: &AppState, id: uuid::Uuid) -> Response {
    match auth::start_session(&state.pool, id).await {
        Ok(token) => (
            [(header::SET_COOKIE, auth::set_cookie(&token))],
            Redirect::to(&state.links.path("/app")),
        )
            .into_response(),
        Err(error) => {
            tracing::warn!(%error, "cannot sign in a new account");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn failed(state: &AppState, key: &str) -> Response {
    Redirect::to(&format!("{}?error={key}", state.links.path("/signup"))).into_response()
}
