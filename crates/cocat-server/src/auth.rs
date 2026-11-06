//! Sign-in sessions and the cookies that carry them: the same `fastapiusersauth` cookie as the
//! Python server, but holding an opaque random token instead of a JWT. The server keeps only
//! the token's hash, so a logout, a password change, or a deactivation can end a session
//! before it expires. The `COCAT_SECRET` HMAC key is still what signs the mailed links.
//!
//! Cookies minted by the Python server stop working at the cutover: users sign in once more.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use argon2::Argon2;
use argon2::password_hash::PasswordHasher;
use argon2::password_hash::PasswordVerifier;
use argon2::password_hash::phc::PasswordHash;
use axum::Form;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use cookie::{Cookie, SameSite};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use thiserror::Error;
use uuid::Uuid;

use crate::AppState;
use crate::db::{self, DbError};
use crate::openapi::Detail;

pub const COOKIE_NAME: &str = "fastapiusersauth";
pub const SECRET_ENV: &str = "COCAT_SECRET";

/// fastapi-users' error codes, which its login route sends as `{"detail": "<code>"}`.
const BAD_CREDENTIALS: &str = "LOGIN_BAD_CREDENTIALS";
const NOT_VERIFIED: &str = "LOGIN_USER_NOT_VERIFIED";

/// What an unknown address is checked against, made once at the first miss with the same
/// parameters as a real hash: a miss and a wrong password must cost the same, or the login
/// timing tells which addresses have accounts. `None` only if argon2 cannot hash at all.
static DUMMY_HASH: LazyLock<Option<String>> = LazyLock::new(|| {
    Argon2::default()
        .hash_password(b"cocat dummy password")
        .ok()
        .map(|hash| hash.to_string())
});

/// The dummy hash a login for an unknown address spends its time on.
pub fn dummy_hash() -> Option<&'static str> {
    DUMMY_HASH.as_deref()
}

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
    #[error("cannot draw a session token: {0}")]
    Random(#[from] getrandom::Error),
    #[error("the password task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
    #[error(transparent)]
    Db(#[from] DbError),
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
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

/// Start a session and return the token for the cookie; only its hash is stored.
pub async fn start_session(pool: &SqlitePool, user_id: Uuid) -> Result<String, AuthError> {
    let (token, hash) = crate::invitations::new_token()?;
    db::create_session(pool, &hash, user_id).await?;
    Ok(token)
}

/// End the session the request's cookie names, if it names one.
pub async fn end_session(pool: &SqlitePool, headers: &HeaderMap) -> Result<(), AuthError> {
    let Some(token) = session_cookie(headers) else {
        return Ok(());
    };
    db::delete_session(pool, &crate::invitations::hash(&token)).await?;
    Ok(())
}

/// Sign one claim set with the server secret. Every token kind is one of these and is told
/// apart by its audience, so a link of one kind can never be spent as another.
pub fn signed_claims<T: Serialize>(claims: &T, secret: &str) -> Result<String, AuthError> {
    encode(
        &Header::new(Algorithm::HS256),
        claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|_| AuthError::BadToken)
}

/// Read a claim set that was signed for `audience` and has not expired.
///
/// pyjwt, which the Python server uses, applies no leeway, and both servers share the secret:
/// a link that expires at a given instant must stop working at that instant on either.
pub fn read_claims<T: DeserializeOwned>(
    token: &str,
    secret: &str,
    audience: &str,
) -> Result<T, AuthError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_audience(&[audience]);
    validation.leeway = 0;
    decode::<T>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map(|data| data.claims)
    .map_err(|_| AuthError::BadToken)
}

pub fn session_cookie(headers: &HeaderMap) -> Option<String> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    Cookie::split_parse(cookies)
        .filter_map(Result::ok)
        .find(|cookie| cookie.name() == COOKIE_NAME)
        .map(|cookie| cookie.value().to_owned())
}

pub async fn authenticate(pool: &SqlitePool, headers: &HeaderMap) -> Result<Uuid, AuthError> {
    let token = session_cookie(headers).ok_or(AuthError::MissingCookie)?;
    let user = db::session_user(pool, &crate::invitations::hash(&token))
        .await?
        .ok_or(AuthError::BadToken)?;
    if !user.is_active {
        return Err(AuthError::InactiveUser);
    }
    Ok(user.id)
}

/// Check the password and start a session.
///
/// `requires_verification` is Python's `requires_verification=mail is not None`: a deployment
/// that cannot mail a confirmation must not lock out the accounts it cannot confirm. The
/// verification is checked after the password, so a wrong password reads as bad credentials
/// for a verified and an unverified account alike.
pub async fn login(
    pool: &SqlitePool,
    email: &str,
    password: &str,
    requires_verification: bool,
) -> Result<String, AuthError> {
    let Some(user) = db::user_by_email(pool, email).await? else {
        // fastapi-users hashes on a miss too, at the same cost as a real hash.
        if let Some(dummy) = dummy_hash() {
            verify_password(password, dummy).await?;
        }
        return Err(AuthError::BadCredentials);
    };
    let needs_rehash = is_bcrypt(&user.hashed_password);
    verify_password(password, &user.hashed_password).await?;
    if !user.is_active {
        return Err(AuthError::InactiveUser);
    }
    if requires_verification && !user.is_verified {
        return Err(AuthError::NotVerified);
    }
    if needs_rehash {
        // A failed rewrite leaves a hash that still works, so it never blocks the sign-in.
        let mut updated = user.clone();
        updated.hashed_password = crate::db::hash_password(password).await?;
        if let Err(error) = db::save_user(pool, &updated).await {
            tracing::warn!(%error, "cannot rehash a password");
        }
    }
    start_session(pool, user.id).await
}

/// Check a password on the blocking pool, so the CPU argon2 wants never stops an async
/// worker from answering other requests.
pub async fn verify_password(password: &str, hashed: &str) -> Result<(), AuthError> {
    let (password, hashed) = (password.to_owned(), hashed.to_owned());
    let matches = crate::hashing::off_worker(move || password_matches(&password, &hashed))
        .await
        .map_err(AuthError::Task)?;
    if matches {
        Ok(())
    } else {
        Err(AuthError::BadCredentials)
    }
}

fn password_matches(password: &str, hashed: &str) -> bool {
    if is_bcrypt(hashed) {
        return bcrypt::verify(password, hashed).unwrap_or(false);
    }
    let Ok(hash) = PasswordHash::new(hashed) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &hash)
        .is_ok()
}

fn is_bcrypt(hashed: &str) -> bool {
    hashed.starts_with("$2a$") || hashed.starts_with("$2b$") || hashed.starts_with("$2y$")
}

pub fn set_cookie(token: &str) -> String {
    session_cookie_conf(token, None).to_string()
}

/// The same attributes the login cookie sets, with the empty value and `Max-Age=0` that
/// Starlette's `CookieTransport` sends on logout.
pub fn clear_cookie() -> String {
    session_cookie_conf("", Some(cookie::time::Duration::ZERO)).to_string()
}

fn session_cookie_conf(token: &str, max_age: Option<cookie::time::Duration>) -> Cookie<'static> {
    let builder = Cookie::build((COOKIE_NAME, token.to_owned()))
        .path("/")
        .secure(true)
        .http_only(true)
        .same_site(SameSite::Lax);
    match max_age {
        Some(max_age) => builder.max_age(max_age).build(),
        None => builder.build(),
    }
}

fn refused(code: &'static str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        axum::Json(serde_json::json!({ "detail": code })),
    )
        .into_response()
}

#[utoipa::path(
    post,
    path = "/auth/jwt/login",
    request_body(content = LoginForm, content_type = "application/x-www-form-urlencoded"),
    responses(
        (status = 204, description = "Signed in: the session cookie is set"),
        (status = 400, description = "Wrong password, inactive account, or unverified address", body = Detail),
        (status = 429, description = "Too many attempts", body = Detail),
    )
)]
pub async fn login_handler(State(state): State<AppState>, Form(form): Form<LoginForm>) -> Response {
    match login(
        &state.pool,
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
#[utoipa::path(
    post,
    path = "/auth/jwt/logout",
    security(("cookie" = [])),
    responses(
        (status = 204, description = "Signed out: the session is ended and the cookie cleared"),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The address is not verified", body = Detail),
    )
)]
pub async fn logout_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Ok(user_id) = authenticate(&state.pool, &headers).await else {
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
    if let Err(error) = end_session(&state.pool, &headers).await {
        tracing::warn!(%error, "cannot end the session");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
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

/// The session routes as the OpenAPI document lists them; the live router is `ws::router`.
pub(crate) fn documented() -> utoipa_axum::router::OpenApiRouter<AppState> {
    utoipa_axum::router::OpenApiRouter::new()
        .routes(utoipa_axum::routes!(login_handler))
        .routes(utoipa_axum::routes!(logout_handler))
}
