//! Forgot-password and reset-password pages.

use std::collections::HashMap;
use std::net::SocketAddr;

use askama::Template;
use axum::extract::{ConnectInfo, Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

use crate::ui::frame::{self, Frame};
use crate::ui::token_from_text;
use crate::{AppState, accounts};

const EMPTY: &str = "Enter a new password";
const MISMATCH: &str = "The new passwords differ";
const INVALID: &str = "This reset link is not valid anymore.";

#[derive(Debug, Deserialize)]
pub(crate) struct ForgotForm {
    email: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResetForm {
    #[serde(default)]
    token: String,
    #[serde(default)]
    link: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    repeat: String,
}

#[derive(Template)]
#[template(path = "forgot.html")]
struct ForgotPage {
    frame: Frame,
    action: String,
    message: &'static str,
}

#[derive(Template)]
#[template(path = "reset.html")]
struct ResetPage {
    frame: Frame,
    action: String,
    message: String,
}

pub(crate) async fn forgot_page(
    Query(query): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> Response {
    frame::page(ForgotPage {
        frame: Frame::new(&state.links, None, "Forgot password"),
        action: state.links.path("/forgot-password"),
        message: frame::message(&query),
    })
}

pub(crate) async fn forgot(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<ForgotForm>,
) -> Response {
    if !state
        .login_limit
        .hit("/ui/forgot-password", &headers, Some(peer))
    {
        return to_forgot(&state, "error=too_many");
    }
    let background = state.clone();
    tokio::spawn(async move { accounts::forgot_password(&background, &form.email).await });
    to_forgot(&state, "sent=1")
}

pub(crate) async fn reset_page(State(state): State<AppState>) -> Response {
    frame::page(ResetPage {
        frame: Frame::new(&state.links, None, "Choose a new password"),
        action: state.links.path("/reset-password"),
        message: String::new(),
    })
}

pub(crate) async fn reset(State(state): State<AppState>, Form(form): Form<ResetForm>) -> Response {
    if form.password.is_empty() {
        return rejected(&state, EMPTY, StatusCode::OK);
    }
    if form.password != form.repeat {
        return rejected(&state, MISMATCH, StatusCode::OK);
    }
    match accounts::reset_password(&state, &form.wanted_token(), &form.password).await {
        Ok(()) => Redirect::to(&format!("{}?reset=1", state.links.path("/login"))).into_response(),
        Err(accounts::AccountError::BadResetToken) => {
            rejected(&state, INVALID, StatusCode::BAD_REQUEST)
        }
        Err(accounts::AccountError::WeakPassword(refusal)) => {
            rejected(&state, &refusal.0, StatusCode::OK)
        }
        Err(error) => {
            tracing::warn!(%error, "cannot reset a password");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

impl ResetForm {
    /// The hidden field carries the fragment; the visible box takes a whole pasted link.
    fn wanted_token(&self) -> String {
        let hidden = self.token.trim();
        if !hidden.is_empty() {
            return hidden.to_owned();
        }
        token_from_text(&self.link)
    }
}

fn to_forgot(state: &AppState, query: &str) -> Response {
    Redirect::to(&format!("{}?{query}", state.links.path("/forgot-password"))).into_response()
}

fn rejected(state: &AppState, message: &str, status: StatusCode) -> Response {
    let page = ResetPage {
        frame: Frame::new(&state.links, None, "Choose a new password"),
        action: state.links.path("/reset-password"),
        message: message.to_owned(),
    };
    (status, frame::page(page)).into_response()
}
