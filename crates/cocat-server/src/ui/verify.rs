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

/// What the verification came to, and the one way forward from there.
struct Answer {
    text: &'static str,
    done: bool,
    href: String,
    label: &'static str,
}

#[derive(Template)]
#[template(path = "verify.html")]
struct VerifyPage {
    frame: Frame,
    action: String,
    answer: Option<Answer>,
}

impl VerifyPage {
    fn new(state: &AppState, answer: Option<Answer>) -> Self {
        Self {
            frame: Frame::new(&state.links, None, "Verify your address"),
            action: state.links.path("/verify"),
            answer,
        }
    }
}

/// The landing page: it only posts the token the link carries in its fragment.
pub(crate) async fn page(State(state): State<AppState>) -> Response {
    frame::page(VerifyPage::new(&state, None))
}

pub(crate) async fn submit(
    State(state): State<AppState>,
    Form(form): Form<VerifyForm>,
) -> Response {
    let text = match form.token.trim() {
        "" => MISSING,
        token => said(&state, token).await,
    };
    let answer = answer(&state, text);
    frame::page(VerifyPage::new(&state, Some(answer)))
}

fn answer(state: &AppState, text: &'static str) -> Answer {
    let done = text == VERIFIED || text == ALREADY;
    let (target, label) = if done {
        ("/rooms", "Continue to my rooms")
    } else {
        ("/login", "Sign in to get a new link")
    };
    Answer {
        text,
        done,
        href: state.links.path(target),
        label,
    }
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
