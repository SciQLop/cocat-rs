//! The account routes fastapi-users serves at the root: registration, the two mailed links
//! and their requests. Logout lives in `auth` because it is the cookie's business.

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::routing::{get, patch, post};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::{ApiError, CurrentUser, JsonBody, UserRead};
use crate::mail::{self, Address};
use crate::ratelimit;
use crate::{AppState, accounts, db};

/// The routes that cost a mail or a password hash share the login limiter, one budget per
/// path; confirming a link and spending one are cheap and stay unlimited.
pub fn router(state: &AppState) -> Router<AppState> {
    let limited = Router::new()
        .route("/auth/register", post(register))
        .route("/auth/request-verify-token", post(request_verify_token))
        .route("/auth/forgot-password", post(forgot_password))
        .route("/users/me", patch(update_me))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            ratelimit::limit,
        ));
    limited
        .route("/auth/verify", post(verify))
        .route("/auth/reset-password", post(reset_password))
        .route("/users/me", get(me))
        .route(
            "/users/{user_id}",
            get(read_user).patch(update_user).delete(delete_user),
        )
}

#[derive(Debug, Deserialize)]
struct RegisterBody {
    email: RegisterEmail,
    password: String,
}

/// Python's `UserCreate` refuses a reserved placeholder in a field validator, so the address
/// is settled while the body is deserialized: a malformed one and a placeholder are both the
/// same 422 as any other schema error, and neither creates a row.
#[derive(Debug, Deserialize)]
#[serde(try_from = "String")]
struct RegisterEmail(String);

impl TryFrom<String> for RegisterEmail {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let address = Address::try_from(value)?;
        if mail::is_placeholder(&address.0) {
            return Err("that address is reserved");
        }
        Ok(Self(address.0))
    }
}

/// 201 with the account, and no cookie: fastapi-users does not sign the new user in.
async fn register(
    State(state): State<AppState>,
    JsonBody(body): JsonBody<RegisterBody>,
) -> Result<(StatusCode, Json<UserRead>), ApiError> {
    let id = accounts::register(&state, &body.email.0, &body.password).await?;
    let user = db::user_by_id(&state.pool, id)
        .await?
        .ok_or(ApiError::Internal)?;
    Ok((StatusCode::CREATED, Json(UserRead::from(user))))
}

#[derive(Debug, Deserialize)]
struct EmailBody {
    email: Address,
}

/// 202 whatever the address is: the reply must not tell a stranger's address from a member's.
async fn request_verify_token(
    State(state): State<AppState>,
    JsonBody(body): JsonBody<EmailBody>,
) -> StatusCode {
    accounts::request_verification(&state, &body.email.0).await;
    StatusCode::ACCEPTED
}

async fn forgot_password(
    State(state): State<AppState>,
    JsonBody(body): JsonBody<EmailBody>,
) -> StatusCode {
    accounts::forgot_password(&state, &body.email.0).await;
    StatusCode::ACCEPTED
}

#[derive(Debug, Deserialize)]
struct TokenBody {
    token: String,
}

async fn verify(
    State(state): State<AppState>,
    JsonBody(body): JsonBody<TokenBody>,
) -> Result<Json<UserRead>, ApiError> {
    let user = accounts::verify(&state, &body.token).await?;
    Ok(Json(UserRead::from(user)))
}

#[derive(Debug, Deserialize)]
struct ResetBody {
    token: String,
    password: String,
}

async fn reset_password(
    State(state): State<AppState>,
    JsonBody(body): JsonBody<ResetBody>,
) -> Result<StatusCode, ApiError> {
    accounts::reset_password(&state, &body.token, &body.password).await?;
    Ok(StatusCode::OK)
}

async fn me(CurrentUser(user): CurrentUser) -> Json<UserRead> {
    Json(UserRead::from(user))
}

/// What a profile edit may say. The flags are not read here, so a request that names them
/// changes nothing: serde drops the unknown fields before the handler sees the body.
#[derive(Debug, Deserialize)]
struct ProfileBody {
    email: Option<Address>,
    password: Option<String>,
}

/// What a superuser edit may say, the same body with the flags this time.
#[derive(Debug, Deserialize)]
struct AdminBody {
    email: Option<Address>,
    password: Option<String>,
    is_active: Option<bool>,
    is_superuser: Option<bool>,
    is_verified: Option<bool>,
}

async fn update_me(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    JsonBody(body): JsonBody<ProfileBody>,
) -> Result<Json<UserRead>, ApiError> {
    let change = accounts::Change {
        email: body.email.map(|address| address.0),
        password: body.password,
        ..accounts::Change::default()
    };
    let user = accounts::change(&state, user, change).await?;
    Ok(Json(UserRead::from(user)))
}

async fn read_user(
    State(state): State<AppState>,
    CurrentUser(actor): CurrentUser,
    Path(raw): Path<String>,
) -> Result<Json<UserRead>, ApiError> {
    superuser(&actor)?;
    let user = find(&state, &raw).await?;
    Ok(Json(UserRead::from(user)))
}

async fn update_user(
    State(state): State<AppState>,
    CurrentUser(actor): CurrentUser,
    Path(raw): Path<String>,
    JsonBody(body): JsonBody<AdminBody>,
) -> Result<Json<UserRead>, ApiError> {
    superuser(&actor)?;
    let user = find(&state, &raw).await?;
    let change = accounts::Change {
        email: body.email.map(|address| address.0),
        password: body.password,
        is_active: body.is_active,
        is_superuser: body.is_superuser,
        is_verified: body.is_verified,
    };
    let user = accounts::change(&state, user, change).await?;
    Ok(Json(UserRead::from(user)))
}

async fn delete_user(
    State(state): State<AppState>,
    CurrentUser(actor): CurrentUser,
    Path(raw): Path<String>,
) -> Result<StatusCode, ApiError> {
    superuser(&actor)?;
    let id = parse_id(&raw)?;
    if !accounts::delete(&state, id).await? {
        return Err(not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The admin routes answer 403 before they look at the id, like fastapi-users, whose
/// superuser dependency runs before the path is parsed.
fn superuser(actor: &db::User) -> Result<(), ApiError> {
    if actor.is_superuser {
        Ok(())
    } else {
        Err(ApiError::Forbidden("Forbidden".to_owned()))
    }
}

/// fastapi-users turns an id that does not parse into the same 404 as one that does not exist.
fn parse_id(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw).map_err(|_| not_found())
}

async fn find(state: &AppState, raw: &str) -> Result<db::User, ApiError> {
    let id = parse_id(raw)?;
    db::user_by_id(&state.pool, id).await?.ok_or_else(not_found)
}

fn not_found() -> ApiError {
    ApiError::NotFound("Not found".to_owned())
}
