//! The account page and the address-confirmation page it links to.

use askama::Template;
use axum::Form;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

use crate::ui::frame::{self, Frame, RequireUser};
use crate::{AppState, accounts, auth, db, mail, oidc};

const EMPTY_PASSWORD: &str = "Enter a new password";
const PASSWORD_MISMATCH: &str = "The new passwords differ";
const WRONG_PASSWORD: &str = "Wrong password";

const EMPTY_EMAIL: &str = "Enter your email address";
const MALFORMED_EMAIL: &str = "That is not an email address";
const TAKEN_EMAIL: &str = "That address belongs to an existing account: sign in with it and \
                            reset its password if you do not have one";
const CONFIRMED: &str = "Your address is confirmed.";

#[derive(Debug, Deserialize)]
pub(crate) struct PasswordForm {
    #[serde(default)]
    current: String,
    #[serde(default)]
    new: String,
    #[serde(default)]
    repeat: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AddressForm {
    #[serde(default)]
    email: String,
}

#[derive(Template)]
#[template(path = "account.html")]
struct AccountPage {
    frame: Frame,
    email: String,
    confirm_href: Option<String>,
    signs_in_with: Option<String>,
    action: String,
    message: String,
}

#[derive(Template)]
#[template(path = "confirm_email.html")]
struct ConfirmEmailPage {
    frame: Frame,
    action: String,
    message: String,
}

pub(crate) async fn page(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
) -> Response {
    account_page(&state, &user, "").await
}

pub(crate) async fn password(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<PasswordForm>,
) -> Response {
    if !state
        .login_limit
        .hit("/ui/account/password", &headers, Some(peer))
    {
        return too_many(&state, &user).await;
    }
    if form.new.is_empty() {
        return account_page(&state, &user, EMPTY_PASSWORD).await;
    }
    if form.new != form.repeat {
        return account_page(&state, &user, PASSWORD_MISMATCH).await;
    }
    if let Err(error) = auth::verify_password(&form.current, &user.hashed_password).await {
        return match error {
            auth::AuthError::BadCredentials => account_page(&state, &user, WRONG_PASSWORD).await,
            error => {
                tracing::warn!(%error, "cannot check the current password");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        };
    }
    let change = accounts::Change {
        password: Some(form.new),
        ..accounts::Change::default()
    };
    match accounts::change(&state, user.clone(), change).await {
        Ok(_) => password_changed(&state),
        Err(accounts::AccountError::WeakPassword(refusal)) => {
            account_page(&state, &user, &refusal.0).await
        }
        Err(error) => {
            tracing::warn!(%error, "cannot change a password");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// The form's budget is spent: the page says why, with the status the API would answer.
async fn too_many(state: &AppState, user: &db::User) -> Response {
    let mut response = account_page(state, user, frame::text("too_many")).await;
    *response.status_mut() = StatusCode::TOO_MANY_REQUESTS;
    response
}

/// The change ended every session, this one included, so the browser is sent back to the
/// sign-in page with the reason.
fn password_changed(state: &AppState) -> Response {
    (
        [(header::SET_COOKIE, auth::clear_cookie())],
        Redirect::to(&format!(
            "{}?error=password_changed",
            state.links.path("/login")
        )),
    )
        .into_response()
}

pub(crate) async fn confirm_page(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
) -> Response {
    if !mail::is_placeholder(&user.email) {
        return Redirect::temporary(&state.links.path("/account")).into_response();
    }
    confirm_email_page(&state, &user, "")
}

pub(crate) async fn confirm(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Form(form): Form<AddressForm>,
) -> Response {
    if !mail::is_placeholder(&user.email) {
        return Redirect::temporary(&state.links.path("/account")).into_response();
    }
    let wanted = form.email.trim();
    if wanted.is_empty() || mail::is_placeholder(wanted) {
        return confirm_email_page(&state, &user, EMPTY_EMAIL);
    }
    let Ok(address) = mail::Address::try_from(wanted.to_owned()) else {
        return confirm_email_page(&state, &user, MALFORMED_EMAIL);
    };
    let change = accounts::Change {
        email: Some(address.0),
        is_verified: Some(state.mailer.is_none()),
        ..accounts::Change::default()
    };
    match accounts::change(&state, user.clone(), change).await {
        Ok(updated) => {
            let message = match state.mailer {
                Some(_) => format!("Check your inbox to verify {}.", updated.email),
                None => CONFIRMED.to_owned(),
            };
            confirm_email_page(&state, &updated, &message)
        }
        Err(accounts::AccountError::EmailInUse) => confirm_email_page(&state, &user, TAKEN_EMAIL),
        Err(error) => {
            tracing::warn!(%error, "cannot change an email address");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn account_page(state: &AppState, user: &db::User, message: &str) -> Response {
    let signs_in_with = match oidc::linked_providers(&state.pool, user.id).await {
        Ok(providers) if !providers.is_empty() => Some(providers.join(", ")),
        Ok(_) => None,
        Err(error) => {
            tracing::warn!(%error, "cannot read the linked providers");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    frame::page(AccountPage {
        frame: Frame::new(&state.links, Some(user), "Account"),
        email: user.email.clone(),
        confirm_href: mail::is_placeholder(&user.email).then(|| state.links.path("/confirm-email")),
        signs_in_with,
        action: state.links.path("/account/password"),
        message: message.to_owned(),
    })
}

fn confirm_email_page(state: &AppState, user: &db::User, message: &str) -> Response {
    frame::page(ConfirmEmailPage {
        frame: Frame::new(&state.links, Some(user), "Confirm your email"),
        action: state.links.path("/confirm-email"),
        message: message.to_owned(),
    })
}
