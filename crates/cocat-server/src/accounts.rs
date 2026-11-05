//! The account lifecycle steps the REST API and the CLI share: registration, the personal
//! room that comes with it, and the verification mail.

use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::AppState;
use crate::db::{self, DbError};
use crate::mail::{self, Mailer};

/// The audience that keeps a verification token from being spent as a reset token.
const VERIFY_AUDIENCE: &str = "cocat:verify";
const VERIFY_LIFETIME_SECONDS: u64 = 3600;

#[derive(Debug, Error)]
pub enum AccountError {
    #[error("an account already exists for that address")]
    Duplicate,
    #[error("the clock is before the unix epoch")]
    Clock,
    #[error(transparent)]
    Token(#[from] jsonwebtoken::errors::Error),
    #[error(transparent)]
    Db(#[from] DbError),
}

/// Create the account and the room it owns.
///
/// Python creates the account unverified and then lets `start_verification` decide: a
/// deployment with no way to send mail verifies it on the spot, because requiring a
/// confirmation would lock the user out of the account it just made. Both steps are one
/// decision here, so the row is written with the answer. The mail itself is best effort: a
/// relay that refuses the message must not fail the registration, and the caller's reply is
/// the same either way.
pub async fn register(state: &AppState, email: &str, password: &str) -> Result<Uuid, AccountError> {
    let id = db::create_user(&state.pool, email, password, false, state.mailer.is_none())
        .await
        .map_err(|error| match error {
            DbError::EmailTaken { .. } => AccountError::Duplicate,
            other => AccountError::Db(other),
        })?;
    db::create_personal_room(&state.pool, id).await?;
    if let Some(mailer) = &state.mailer {
        send_verification(state, mailer, id, email).await;
    }
    Ok(id)
}

async fn send_verification(state: &AppState, mailer: &Mailer, id: Uuid, email: &str) {
    if mail::is_placeholder(email) {
        return;
    }
    let token = match verification_token(id, email, &state.secret, SystemTime::now()) {
        Ok(token) => token,
        Err(error) => {
            tracing::warn!(%error, "cannot build a verification token");
            return;
        }
    };
    let (subject, body) = mail::verification_mail(&state.links.ui(&format!("/verify#{token}")));
    if let Err(error) = mailer.send(email, subject, &body).await {
        // The token must never reach the log, so only the failure is recorded.
        tracing::warn!(%error, "could not mail the verification link");
    }
}

#[derive(Debug, Serialize)]
struct VerifyClaims {
    sub: String,
    email: String,
    aud: String,
    exp: i64,
}

/// A short-lived link token: the subject and the address it was issued for, in one audience
/// of its own, so the reset route (another audience) can never accept it.
fn verification_token(
    id: Uuid,
    email: &str,
    secret: &str,
    now: SystemTime,
) -> Result<String, AccountError> {
    let issued_at = now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AccountError::Clock)?
        .as_secs();
    let claims = VerifyClaims {
        sub: id.to_string(),
        email: email.to_owned(),
        aud: VERIFY_AUDIENCE.to_owned(),
        exp: i64::try_from(issued_at + VERIFY_LIFETIME_SECONDS).map_err(|_| AccountError::Clock)?,
    };
    Ok(encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )?)
}
