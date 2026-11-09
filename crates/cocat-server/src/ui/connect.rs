//! The page a browser sign-in link opens: the person checks the code the app shows and allows
//! or denies it (see `sign_in`).

use std::collections::HashMap;

use askama::Template;
use axum::Form;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::sign_in::{self, Waiting};
use crate::ui::frame::{self, Frame, RequireUser};
use crate::{AppState, db};

const EXPIRED_HEADING: &str = "Link expired";
const EXPIRED: &str = "This sign-in link expired or was already used. Ask the app for a new one.";

#[derive(Debug, Deserialize)]
pub(crate) struct DecisionForm {
    #[serde(default)]
    code: String,
    #[serde(default)]
    decision: String,
}

#[derive(Template)]
#[template(path = "connect.html")]
struct ConnectPage {
    frame: Frame,
    action: String,
    account_href: String,
    waiting: Option<Waiting>,
    heading: String,
    outcome: String,
}

pub(crate) async fn page(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let code = query.get("code").map(String::as_str).unwrap_or_default();
    match sign_in::waiting(&state.pool, code).await {
        Ok(waiting @ Some(_)) => render(&state, &user, waiting, "", ""),
        Ok(None) => render(&state, &user, None, EXPIRED_HEADING, EXPIRED),
        Err(error) => failed(error),
    }
}

pub(crate) async fn decide(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Form(form): Form<DecisionForm>,
) -> Response {
    let allow = form.decision == "allow";
    match sign_in::decide(&state.pool, &form.code, user.id, allow).await {
        Ok(Some(name)) if allow => render(
            &state,
            &user,
            None,
            "App signed in",
            &format!("{name} is signed in. You can go back to it."),
        ),
        Ok(Some(name)) => render(
            &state,
            &user,
            None,
            "App denied",
            &format!("{name} was not allowed. It cannot use your account."),
        ),
        Ok(None) => render(&state, &user, None, EXPIRED_HEADING, EXPIRED),
        Err(error) => failed(error),
    }
}

fn render(
    state: &AppState,
    user: &db::User,
    waiting: Option<Waiting>,
    heading: &str,
    outcome: &str,
) -> Response {
    frame::page(ConnectPage {
        frame: Frame::new(&state.links, Some(user), "Sign in an app"),
        action: state.links.path("/connect"),
        account_href: state.links.path("/account"),
        waiting,
        heading: heading.to_owned(),
        outcome: outcome.to_owned(),
    })
}

fn failed(error: sign_in::SignInError) -> Response {
    tracing::warn!(%error, "cannot answer a sign-in request");
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}
