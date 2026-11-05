//! Tokens and cookies compatible with the Python server (fastapi-users): the same
//! `fastapiusersauth` cookie, the same HMAC secret from `COCAT_SECRET` or
//! `<db_path>.secret`, and the same claims, so a cookie one server mints the other accepts.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use argon2::Argon2;
use argon2::password_hash::PasswordVerifier;
use argon2::password_hash::phc::PasswordHash;
use axum::Form;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use thiserror::Error;
use uuid::Uuid;

use crate::AppState;
use crate::db::{self, DbError};

pub const COOKIE_NAME: &str = "fastapiusersauth";
pub const SECRET_ENV: &str = "COCAT_SECRET";

const AUDIENCE: &str = "fastapi-users:auth";

/// fastapi-users' error codes, which its login route sends as `{"detail": "<code>"}`.
const BAD_CREDENTIALS: &str = "LOGIN_BAD_CREDENTIALS";
const NOT_VERIFIED: &str = "LOGIN_USER_NOT_VERIFIED";
const LIFETIME_SECONDS: u64 = 3600;
const DUMMY_HASH: &str = "$argon2id$v=19$m=65536,t=3,p=4$YGXUcrLDsAk2cwaSUXo9kg$kDCnwbFPV9Go5Cw1ADnZFM0j/t7ythVDk0kxfS6Ve+Q";

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("no fastapiusersauth cookie")]
    MissingCookie,
    #[error("bad token")]
    BadToken,
    #[error("user is inactive")]
    InactiveUser,
    #[error("bad credentials")]
    BadCredentials,
    #[error("the account is not verified")]
    NotVerified,
    #[error("cannot read the token secret at {path}: {source}")]
    Secret {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Db(#[from] DbError),
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    aud: Vec<String>,
    exp: i64,
}

#[derive(Debug, Deserialize)]
pub struct LoginForm {
    pub username: String,
    pub password: String,
}

pub fn load_secret(db_path: &Path) -> Result<String, AuthError> {
    resolve_secret(env::var(SECRET_ENV).ok(), db_path)
}

pub fn resolve_secret(environment: Option<String>, db_path: &Path) -> Result<String, AuthError> {
    match environment.filter(|secret| !secret.is_empty()) {
        Some(secret) => Ok(secret),
        None => file_secret(&secret_path(db_path)),
    }
}

fn file_secret(path: &Path) -> Result<String, AuthError> {
    let secret = match fs::read_to_string(path) {
        Ok(secret) => secret,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_secret(path)?;
            fs::read_to_string(path).map_err(|source| secret_error(path, source))?
        }
        Err(source) => return Err(secret_error(path, source)),
    };
    let secret = secret.trim();
    if secret.is_empty() {
        let source = io::Error::new(io::ErrorKind::InvalidData, "secret file is empty");
        return Err(secret_error(path, source));
    }
    Ok(secret.to_owned())
}

fn create_secret(path: &Path) -> Result<(), AuthError> {
    let temporary = temporary_secret_path(path);
    let written = write_secret(&temporary, path, &new_secret());
    let _ = fs::remove_file(&temporary);
    written.map_err(|source| secret_error(path, source))
}

fn write_secret(temporary: &Path, path: &Path, secret: &str) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(temporary)?;
    file.write_all(secret.as_bytes())?;
    file.sync_all()?;
    match fs::hard_link(temporary, path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

// simplify: two v4 UUIDs (244 bits) instead of a base64 32-byte token; the file is opaque
// to Python, which only reads it back and signs with it.
fn new_secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn temporary_secret_path(path: &Path) -> PathBuf {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", Uuid::new_v4().simple()));
    PathBuf::from(temporary)
}

fn secret_error(path: &Path, source: io::Error) -> AuthError {
    AuthError::Secret {
        path: path.to_owned(),
        source,
    }
}

pub fn mint_token(user_id: Uuid, secret: &str, now: SystemTime) -> Result<String, AuthError> {
    let issued_at = now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AuthError::BadToken)?
        .as_secs();
    let claims = Claims {
        sub: user_id.to_string(),
        aud: vec![AUDIENCE.to_owned()],
        exp: i64::try_from(issued_at + LIFETIME_SECONDS).map_err(|_| AuthError::BadToken)?,
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|_| AuthError::BadToken)
}

pub fn verify_token(token: &str, secret: &str) -> Result<Uuid, AuthError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_audience(&[AUDIENCE]);
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map_err(|_| AuthError::BadToken)?;
    Uuid::parse_str(&data.claims.sub).map_err(|_| AuthError::BadToken)
}

pub fn session_cookie(headers: &HeaderMap) -> Option<String> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    cookies
        .split(';')
        .filter_map(|cookie| cookie.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE_NAME)
        .map(|(_, value)| value.to_owned())
}

pub async fn authenticate(
    pool: &SqlitePool,
    headers: &HeaderMap,
    secret: &str,
) -> Result<Uuid, AuthError> {
    let token = session_cookie(headers).ok_or(AuthError::MissingCookie)?;
    let user_id = verify_token(&token, secret)?;
    let user = db::user_by_id(pool, user_id)
        .await?
        .ok_or(AuthError::BadToken)?;
    if !user.is_active {
        return Err(AuthError::InactiveUser);
    }
    Ok(user.id)
}

/// Check the password and hand out a session token.
///
/// `requires_verification` is Python's `requires_verification=mail is not None`: a deployment
/// that cannot mail a confirmation must not lock out the accounts it cannot confirm. The
/// verification is checked after the password, so a wrong password reads as bad credentials
/// for a verified and an unverified account alike.
pub async fn login(
    pool: &SqlitePool,
    secret: &str,
    email: &str,
    password: &str,
    requires_verification: bool,
) -> Result<String, AuthError> {
    let Some(user) = db::user_by_email(pool, email).await? else {
        // fastapi-users hashes on a miss too, so an unknown email costs one argon2 run.
        verify_password(password, DUMMY_HASH)?;
        return Err(AuthError::BadCredentials);
    };
    verify_password(password, &user.hashed_password)?;
    if !user.is_active {
        return Err(AuthError::InactiveUser);
    }
    if requires_verification && !user.is_verified {
        return Err(AuthError::NotVerified);
    }
    mint_token(user.id, secret, SystemTime::now())
}

// simplify: only argon2 PHC hashes verify; bcrypt rows from older Python accounts fail
// PasswordHash::new and read as bad credentials. Phase 1 supports bcrypt or forces a reset.
fn verify_password(password: &str, hashed: &str) -> Result<(), AuthError> {
    let hash = PasswordHash::new(hashed).map_err(|_| AuthError::BadCredentials)?;
    Argon2::default()
        .verify_password(password.as_bytes(), &hash)
        .map_err(|_| AuthError::BadCredentials)
}

pub fn set_cookie(token: &str) -> String {
    format!("{COOKIE_NAME}={token}; Path=/; Secure; HttpOnly; SameSite=lax")
}

/// The same attributes the login cookie sets, with the empty value and `Max-Age=0` that
/// Starlette's `CookieTransport` sends on logout.
pub fn clear_cookie() -> String {
    format!("{COOKIE_NAME}=\"\"; HttpOnly; Max-Age=0; Path=/; SameSite=lax; Secure")
}

fn refused(code: &'static str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        axum::Json(serde_json::json!({ "detail": code })),
    )
        .into_response()
}

pub async fn login_handler(State(state): State<AppState>, Form(form): Form<LoginForm>) -> Response {
    match login(
        &state.pool,
        &state.secret,
        &form.username,
        &form.password,
        state.mailer.is_some(),
    )
    .await
    {
        Ok(token) => (
            StatusCode::NO_CONTENT,
            [(header::SET_COOKIE, set_cookie(&token))],
        )
            .into_response(),
        Err(AuthError::BadCredentials | AuthError::InactiveUser) => refused(BAD_CREDENTIALS),
        Err(AuthError::NotVerified) => refused(NOT_VERIFIED),
        Err(error) => {
            tracing::warn!(%error, "login failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Drop the session cookie.
///
/// Python gates this route on the same verification rule as login (`verified=mail is not
/// None`), and its authenticator answers 403 when the cookie is good but the account is
/// unverified, so the two refusals are kept apart.
pub async fn logout_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Ok(user_id) = authenticate(&state.pool, &headers, &state.secret).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if state.mailer.is_some() {
        match db::user_by_id(&state.pool, user_id).await {
            Ok(Some(user)) if user.is_verified => {}
            Ok(_) => return StatusCode::FORBIDDEN.into_response(),
            Err(error) => {
                tracing::warn!(%error, "cannot read the user who is logging out");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
    }
    (
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, clear_cookie())],
    )
        .into_response()
}

fn secret_path(db_path: &Path) -> PathBuf {
    let mut path = db_path.as_os_str().to_owned();
    path.push(".secret");
    PathBuf::from(path)
}
