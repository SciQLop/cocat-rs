//! The page an invitation link lands on.

use askama::Template;
use axum::Form;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

use crate::ui::frame::{self, Frame, MaybeUser, RequireUser};
use crate::ui::{query_value, token_from_text};
use crate::{AppState, db, invitations, lifecycle};

const INVALID: &str = "This invitation is not valid anymore";
const UNKNOWN_INVITER: &str = "Someone";

#[derive(Debug, Deserialize)]
pub(crate) struct TokenForm {
    #[serde(default)]
    token: String,
    #[serde(default)]
    link: String,
}

impl TokenForm {
    /// The hidden field carries the fragment; the visible box takes a whole pasted link.
    fn wanted(&self) -> String {
        let hidden = self.token.trim();
        if hidden.is_empty() {
            token_from_text(&self.link)
        } else {
            hidden.to_owned()
        }
    }
}

struct Offer {
    inviter: String,
    room_name: String,
    role: &'static str,
    expires: String,
    token: String,
    signed_in: bool,
}

#[derive(Template)]
#[template(path = "invitation.html")]
struct InvitationPage {
    frame: Frame,
    action: String,
    accept_action: String,
    paste: bool,
    submit_on_load: bool,
    message: &'static str,
    login_href: String,
    offer: Option<Offer>,
}

pub(crate) async fn page(State(state): State<AppState>, MaybeUser(user): MaybeUser) -> Response {
    frame::page(asking(&state, user.as_ref()))
}

pub(crate) async fn preview(
    State(state): State<AppState>,
    MaybeUser(user): MaybeUser,
    Form(form): Form<TokenForm>,
) -> Response {
    let wanted = form.wanted();
    if wanted.is_empty() {
        return frame::page(asking(&state, user.as_ref()));
    }
    match invitations::preview(&state, &wanted).await {
        Ok(preview) => {
            let offer = Offer {
                inviter: preview
                    .inviter_email
                    .unwrap_or_else(|| UNKNOWN_INVITER.to_owned()),
                room_name: preview.room_name,
                role: preview.role.as_str(),
                expires: preview
                    .expires_at
                    .get(..10)
                    .unwrap_or(&preview.expires_at)
                    .to_owned(),
                token: wanted,
                signed_in: user.is_some(),
            };
            frame::page(shown(&state, user.as_ref(), "", Some(offer)))
        }
        Err(invalid) => {
            if !matches!(invalid, lifecycle::LifecycleError::NotFound(_)) {
                tracing::warn!(%invalid, "cannot read an invitation");
            }
            frame::page(shown(&state, user.as_ref(), INVALID, None))
        }
    }
}

pub(crate) async fn accept(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Form(form): Form<TokenForm>,
) -> Response {
    let wanted = form.wanted();
    if wanted.is_empty() {
        return Redirect::to(&state.links.path("/invitation")).into_response();
    }
    match invitations::accept(&state, &wanted, &user).await {
        Ok((room, _)) => frame::flash(
            &state.links,
            Redirect::to(&state.links.path(&format!("/rooms/{}", room.id))).into_response(),
            &format!("You joined {}", room.name),
        ),
        Err(error) => refuse(&state, error),
    }
}

fn refuse(state: &AppState, error: lifecycle::LifecycleError) -> Response {
    let text =
        match error {
            lifecycle::LifecycleError::NotFound(text)
            | lifecycle::LifecycleError::Forbidden(text) => text,
            lifecycle::LifecycleError::Conflict(text)
            | lifecycle::LifecycleError::Invalid(text) => text.to_owned(),
            error => {
                tracing::warn!(%error, "cannot accept an invitation");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        };
    let back = Redirect::to(&state.links.path("/invitation")).into_response();
    frame::flash(&state.links, back, &text)
}

fn asking(state: &AppState, user: Option<&db::User>) -> InvitationPage {
    build(state, user, true, true, "", None)
}

fn shown(
    state: &AppState,
    user: Option<&db::User>,
    message: &'static str,
    offer: Option<Offer>,
) -> InvitationPage {
    build(state, user, false, false, message, offer)
}

fn build(
    state: &AppState,
    user: Option<&db::User>,
    paste: bool,
    submit_on_load: bool,
    message: &'static str,
    offer: Option<Offer>,
) -> InvitationPage {
    let login_href = match (user, offer.as_ref()) {
        (None, Some(_)) => format!(
            "{}?next={}",
            state.links.path("/login"),
            query_value(&state.links.path("/invitation"))
        ),
        _ => String::new(),
    };
    InvitationPage {
        frame: Frame::new(&state.links, user, "Join a room"),
        action: state.links.path("/invitation"),
        accept_action: state.links.path("/invitation/accept"),
        paste,
        submit_on_load,
        message,
        login_href,
        offer,
    }
}
