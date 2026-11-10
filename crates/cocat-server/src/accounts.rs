//! The account lifecycle steps the REST API and the CLI share: registration, the personal
//! room that comes with it, and the two mailed links (confirm the address, choose a new
//! password).

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use cocat_core::role::Role;

use crate::AppState;
use crate::auth::{self, AuthError};
use crate::db::{self, DbError, User};
use crate::mail::{self, Mailer};
use crate::password;
use crate::room::CloseReason;

/// The audiences that keep one kind of link from being spent as another.
const VERIFY_AUDIENCE: &str = "cocat:verify";
const RESET_AUDIENCE: &str = "cocat:reset";
const LINK_LIFETIME_SECONDS: u64 = 3600;

#[derive(Debug, Error)]
pub enum AccountError {
    #[error("an account already exists for that address")]
    Duplicate,
    #[error("the clock is before the unix epoch")]
    Clock,
    #[error("the verification token is bad, expired, or for another address")]
    BadVerifyToken,
    #[error("the account is already verified")]
    AlreadyVerified,
    #[error("the reset token is bad, expired, or already spent")]
    BadResetToken,
    #[error("the address belongs to another account")]
    EmailInUse,
    #[error(transparent)]
    WeakPassword(#[from] password::WeakPassword),
    #[error("cannot sign a link")]
    Sign(#[from] AuthError),
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
    password::check(password, email)?;
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

/// Mail a fresh confirmation link whenever an account could use one.
///
/// Without a mailer registration verified every account on the spot, so there is nothing to
/// send and the caller still answers 202.
pub async fn request_verification(state: &AppState, email: &str) {
    let Some(mailer) = &state.mailer else {
        return;
    };
    let Some(user) = owner_of(state, email).await else {
        return;
    };
    if user.is_verified {
        return;
    }
    send_verification(state, mailer, user.id, &user.email).await;
}

/// Tell an address that already has an account that someone tried to sign up with it.
///
/// The sign-up form answers as if the account had just been created, so this mail is the
/// only thing that tells the owner; a deployment with no mailer says the address is taken
/// instead, because it has no way to tell them.
pub async fn notify_signup_attempt(state: &AppState, email: &str) {
    let Some(mailer) = &state.mailer else {
        return;
    };
    let text = mail::signup_attempt_mail(&state.links.ui("/login"));
    deliver(mailer, email, text).await;
}

/// Turn a confirmation link into a verified account.
pub async fn verify(state: &AppState, token: &str) -> Result<User, AccountError> {
    let claims: VerifyClaims = auth::read_claims(token, &state.secret, VERIFY_AUDIENCE)
        .map_err(|_| AccountError::BadVerifyToken)?;
    // Python finds the account by the address in the token and then checks the id, so a link
    // mailed before an address change stops working instead of confirming the new one.
    let user = db::user_by_email(&state.pool, &claims.email)
        .await?
        .ok_or(AccountError::BadVerifyToken)?;
    if claims.sub != user.id.to_string() {
        return Err(AccountError::BadVerifyToken);
    }
    if user.is_verified {
        return Err(AccountError::AlreadyVerified);
    }
    if !db::set_verified(&state.pool, user.id).await? {
        return Err(AccountError::BadVerifyToken);
    }
    Ok(User {
        is_verified: true,
        ..user
    })
}

/// Mail a reset link whenever an account could use one; the caller answers 202 either way.
pub async fn forgot_password(state: &AppState, email: &str) {
    let Some(mailer) = &state.mailer else {
        return;
    };
    let Some(user) = owner_of(state, email).await else {
        return;
    };
    send_reset(state, mailer, &user).await;
}

/// Spend a reset link: the token names the account and the password it was issued against.
/// Browser sessions end; the apps' tokens stay, for the person to revoke on the account page.
pub async fn reset_password(
    state: &AppState,
    token: &str,
    password: &str,
) -> Result<(), AccountError> {
    let claims: ResetClaims = auth::read_claims(token, &state.secret, RESET_AUDIENCE)
        .map_err(|_| AccountError::BadResetToken)?;
    let id = Uuid::parse_str(&claims.sub).map_err(|_| AccountError::BadResetToken)?;
    let user = db::user_by_id(&state.pool, id)
        .await?
        .ok_or(AccountError::BadResetToken)?;
    if claims.password_fgpt != password_fingerprint(&user.hashed_password) {
        return Err(AccountError::BadResetToken);
    }
    if !user.is_active {
        return Err(AccountError::BadResetToken);
    }
    password::check(password, &user.email)?;
    let new_hash = db::hash_password(password).await?;
    let mut tx = db::begin_immediate(&state.pool).await?;
    if !db::replace_password(&mut *tx, user.id, &user.hashed_password, &new_hash).await? {
        return Err(AccountError::BadResetToken);
    }
    // A reset is what someone does when they think the account was taken: unlike a password
    // change, it signs every app out too.
    let ended = db::end_sessions_of(&mut *tx, user.id).await?;
    tx.commit().await.map_err(DbError::from)?;
    ended.iter().for_each(|hash| state.rooms.end_session(hash));
    Ok(())
}

/// The fields an edit may set. Both account routes fill the same struct: the profile route can
/// only reach `email` and `password`, the superuser route fills whichever flag the body names.
#[derive(Debug, Default)]
pub struct Change {
    pub email: Option<String>,
    pub password: Option<String>,
    pub is_active: Option<bool>,
    pub is_superuser: Option<bool>,
    pub is_verified: Option<bool>,
}

/// Apply a profile or admin edit and answer the account as it now stands.
///
/// A new address is unproven until the link mailed to it is opened, unless the caller set
/// `is_verified` explicitly in the same request; that is the one difference between the safe
/// profile route (which never fills the flags) and the superuser one. The mail is sent after
/// the write, so a relay that refuses it cannot fail an update that already happened.
pub async fn change(state: &AppState, user: User, change: Change) -> Result<User, AccountError> {
    let mut updated = user;
    let was_active = updated.is_active;
    let was_superuser = updated.is_superuser;
    let mut new_password = false;
    let moved = change
        .email
        .as_deref()
        .is_some_and(|email| email != updated.email);
    updated.is_active = change.is_active.unwrap_or(updated.is_active);
    updated.is_superuser = change.is_superuser.unwrap_or(updated.is_superuser);
    updated.is_verified = change.is_verified.unwrap_or(updated.is_verified);
    if let Some(email) = change.email {
        updated.email = email;
    }
    if moved && change.is_verified.is_none() {
        updated.is_verified = false;
    }
    if let Some(password) = change.password {
        password::check(&password, &updated.email)?;
        updated.hashed_password = db::hash_password(&password).await?;
        new_password = true;
    }
    let deactivated = was_active && !updated.is_active;
    if new_password || deactivated {
        let mut tx = db::begin_immediate(&state.pool).await?;
        db::save_user(&mut *tx, &updated)
            .await
            .map_err(account_error)?;
        if deactivated {
            let ended = db::end_sessions_of(&mut *tx, updated.id).await?;
            ended.iter().for_each(|hash| state.rooms.end_session(hash));
        } else {
            db::end_browser_sessions_of(&mut *tx, updated.id).await?;
        }
        tx.commit().await.map_err(DbError::from)?;
    } else {
        db::save_user(&state.pool, &updated)
            .await
            .map_err(account_error)?;
    }
    if was_active && !updated.is_active {
        revoke_sockets(
            state,
            updated.id,
            db::list_user_rooms(&state.pool, updated.id).await?,
        )
        .await;
    }
    if deactivated {
        revoke_public(state, updated.id, CloseReason::SessionEnded).await?;
    } else if was_superuser != updated.is_superuser {
        revoke_public(state, updated.id, CloseReason::AccessChanged).await?;
    }
    if moved && let Some(mailer) = &state.mailer {
        send_verification(state, mailer, updated.id, &updated.email).await;
    }
    Ok(updated)
}

/// A duplicate address is the profile route's conflict; anything else is the database's.
fn account_error(error: DbError) -> AccountError {
    match error {
        DbError::EmailTaken { .. } => AccountError::EmailInUse,
        other => AccountError::Db(other),
    }
}

/// Remove the account and then close every socket it still has. The rooms themselves and their
/// files stay, so an owned room outlives its last member, exactly as Python leaves it.
///
/// The rooms are read before the row goes, because the memberships cascade with it; the revokes
/// run afterwards because a socket that attaches in between must find no membership left to
/// pass the bridge's post-attach re-read.
pub async fn delete(state: &AppState, id: Uuid) -> Result<bool, AccountError> {
    let mut tx = db::begin_immediate(&state.pool).await?;
    let rooms = db::list_user_rooms(&mut *tx, id).await?;
    let deleted = db::delete_user(&mut *tx, id).await?;
    tx.commit().await.map_err(DbError::from)?;
    revoke_sockets(state, id, rooms).await;
    revoke_public(state, id, CloseReason::SessionEnded).await?;
    Ok(deleted)
}

/// Close the sockets these memberships gave the account, once the write that took them away has
/// committed.
async fn revoke_sockets(state: &AppState, id: Uuid, rooms: Vec<(db::RoomRow, Role)>) {
    for (room, _) in rooms {
        state
            .rooms
            .revoke_project(&state.pool, &room.id, id, CloseReason::SessionEnded)
            .await;
    }
}

/// Close the account's sockets on every public catalogue, once its role there changed: it
/// became or stopped being a curator, or it can no longer sign in.
pub(crate) async fn revoke_public(
    state: &AppState,
    id: Uuid,
    reason: CloseReason,
) -> Result<(), DbError> {
    for room in db::public_rooms(&state.pool).await? {
        state
            .rooms
            .revoke_project(&state.pool, &room.id, id, reason)
            .await;
    }
    Ok(())
}

/// The account behind an address, or `None` when there is none, it is inactive, or it cannot
/// be read. Both mailed-link requests answer the same way whatever this finds, so a failure to
/// read is logged rather than turned into a reply that would tell a stranger's address apart
/// from a member's.
async fn owner_of(state: &AppState, email: &str) -> Option<User> {
    match db::user_by_email(&state.pool, email).await {
        Ok(Some(user)) if user.is_active && !mail::is_placeholder(&user.email) => Some(user),
        Ok(_) => None,
        Err(error) => {
            tracing::warn!(%error, "cannot look up an address for a mailed link");
            None
        }
    }
}

async fn send_verification(state: &AppState, mailer: &Mailer, id: Uuid, email: &str) {
    if mail::is_placeholder(email) {
        return;
    }
    let token = match verification_token(id, email, &state.secret, SystemTime::now()) {
        Ok(token) => token,
        Err(error) => {
            tracing::warn!(%error, "cannot sign a verification link");
            return;
        }
    };
    let text = mail::verification_mail(&state.links.ui(&format!("/verify#{token}")));
    deliver(mailer, email, text).await;
}

async fn send_reset(state: &AppState, mailer: &Mailer, user: &User) {
    let token = match reset_token(
        user.id,
        &user.hashed_password,
        &state.secret,
        SystemTime::now(),
    ) {
        Ok(token) => token,
        Err(error) => {
            tracing::warn!(%error, "cannot sign a reset link");
            return;
        }
    };
    let text = mail::reset_mail(&state.links.ui(&format!("/reset-password#{token}")));
    deliver(mailer, &user.email, text).await;
}

/// The mail is best effort: a refused message changes neither the reply nor the account, and
/// the token must never reach the log, so only the failure is recorded.
async fn deliver(mailer: &Mailer, to: &str, (subject, body): (&'static str, String)) {
    if let Err(error) = mailer.send(to, subject, &body).await {
        tracing::warn!(%error, "could not mail the link");
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct VerifyClaims {
    sub: String,
    email: String,
    aud: String,
    exp: i64,
}

#[derive(Debug, Deserialize, Serialize)]
struct ResetClaims {
    sub: String,
    password_fgpt: String,
    aud: String,
    exp: i64,
}

/// A confirmation link for the address as it is now, in an audience of its own so the reset
/// route can never accept it.
///
/// `now` is a parameter so a test can mint an expired link without sleeping.
pub fn verification_token(
    id: Uuid,
    email: &str,
    secret: &str,
    now: SystemTime,
) -> Result<String, AccountError> {
    let claims = VerifyClaims {
        sub: id.to_string(),
        email: email.to_owned(),
        aud: VERIFY_AUDIENCE.to_owned(),
        exp: expiry(now)?,
    };
    Ok(auth::signed_claims(&claims, secret)?)
}

/// A reset link that also binds to the password in force when it was issued: changing the
/// password is what spends the link, so a second copy of it stops matching as well.
pub fn reset_token(
    id: Uuid,
    hashed_password: &str,
    secret: &str,
    now: SystemTime,
) -> Result<String, AccountError> {
    let claims = ResetClaims {
        sub: id.to_string(),
        password_fgpt: password_fingerprint(hashed_password),
        aud: RESET_AUDIENCE.to_owned(),
        exp: expiry(now)?,
    };
    Ok(auth::signed_claims(&claims, secret)?)
}

fn expiry(now: SystemTime) -> Result<i64, AccountError> {
    let issued_at = now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AccountError::Clock)?
        .as_secs();
    i64::try_from(issued_at + LINK_LIFETIME_SECONDS).map_err(|_| AccountError::Clock)
}

/// fastapi-users hashes the stored hash with its password helper; any one-way function of it
/// serves the same purpose, and a digest keeps validating a reset link cheap.
fn password_fingerprint(hashed_password: &str) -> String {
    Sha256::digest(hashed_password.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
