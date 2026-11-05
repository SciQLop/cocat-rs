//! Tokens and cookies compatible with the Python server (fastapi-users).
//!
//! simplify: the spike verifies the Python server's tokens and mints the same shape;
//! phase 1 replaces this module with Rust auth.

use std::env;
use std::fs;
use std::io;
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
const LIFETIME_SECONDS: u64 = 3600;

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
    if let Some(secret) = environment {
        return Ok(secret);
    }
    let path = secret_path(db_path);
    let secret = fs::read_to_string(&path).map_err(|source| AuthError::Secret {
        path: path.clone(),
        source,
    })?;
    Ok(secret.trim().to_owned())
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

pub async fn login(
    pool: &SqlitePool,
    secret: &str,
    email: &str,
    password: &str,
) -> Result<String, AuthError> {
    let user = db::user_by_email(pool, email)
        .await?
        .ok_or(AuthError::BadCredentials)?;
    let hash = PasswordHash::new(&user.hashed_password).map_err(|_| AuthError::BadCredentials)?;
    Argon2::default()
        .verify_password(password.as_bytes(), &hash)
        .map_err(|_| AuthError::BadCredentials)?;
    if !user.is_active {
        return Err(AuthError::InactiveUser);
    }
    mint_token(user.id, secret, SystemTime::now())
}

pub fn set_cookie(token: &str) -> String {
    format!("{COOKIE_NAME}={token}; Path=/; Secure; HttpOnly; SameSite=lax")
}

pub async fn login_handler(State(state): State<AppState>, Form(form): Form<LoginForm>) -> Response {
    match login(&state.pool, &state.secret, &form.username, &form.password).await {
        Ok(token) => (
            StatusCode::NO_CONTENT,
            [(header::SET_COOKIE, set_cookie(&token))],
        )
            .into_response(),
        Err(AuthError::BadCredentials | AuthError::InactiveUser) => {
            StatusCode::BAD_REQUEST.into_response()
        }
        Err(error) => {
            tracing::warn!(%error, "login failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn secret_path(db_path: &Path) -> PathBuf {
    let mut path = db_path.as_os_str().to_owned();
    path.push(".secret");
    PathBuf::from(path)
}
