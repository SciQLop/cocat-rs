//! The page a verification link lands on.

use askama::Template;
use axum::extract::{Form, State};
use axum::response::Response;
use serde::Deserialize;

use crate::ui::frame::{self, Frame};
use crate::{AppState, accounts};

const VERIFIED: &str = "Your address is verified.";
const ALREADY: &str = "Your address is already verified.";
const INVALID: &str = "This verification link is not valid anymore.";
const MISSING: &str = "Open the link from your inbox to verify your address.";

#[derive(Debug, Deserialize)]
pub(crate) struct VerifyForm {
    #[serde(default)]
    token: String,
}

#[derive(Template)]
#[template(path = "verify.html")]
struct VerifyPage {
    frame: Frame,
    action: String,
    rooms_href: String,
    message: &'static str,
    submit_on_load: bool,
}

pub(crate) async fn page(State(state): State<AppState>) -> Response {
    let page = VerifyPage {
        frame: Frame::new(&state.links, None, "Verify your address"),
        action: state.links.path("/verify"),
        rooms_href: state.links.path("/rooms"),
        message: MISSING,
        submit_on_load: true,
    };
    frame::page(page)
}

pub(crate) async fn submit(
    State(state): State<AppState>,
    Form(form): Form<VerifyForm>,
) -> Response {
    let message = match form.token.trim() {
        "" => MISSING,
        token => said(&state, token).await,
    };
    let page = VerifyPage {
        frame: Frame::new(&state.links, None, "Verify your address"),
        action: state.links.path("/verify"),
        rooms_href: state.links.path("/rooms"),
        message,
        submit_on_load: false,
    };
    frame::page(page)
}

async fn said(state: &AppState, token: &str) -> &'static str {
    match accounts::verify(state, token).await {
        Ok(_) => VERIFIED,
        Err(accounts::AccountError::AlreadyVerified) => ALREADY,
        Err(accounts::AccountError::BadVerifyToken) => INVALID,
        Err(error) => {
            tracing::warn!(%error, "cannot verify an address");
            INVALID
        }
    }
}
