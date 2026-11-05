//! The account routes fastapi-users serves at the root: registration, the two mailed links
//! and their requests. Logout lives in `auth` because it is the cookie's business.

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware;
use axum::routing::post;
use serde::Deserialize;

use crate::api::{ApiError, JsonBody, UserRead};
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
        .layer(middleware::from_fn_with_state(
            state.clone(),
            ratelimit::limit,
        ));
    limited
        .route("/auth/verify", post(verify))
        .route("/auth/reset-password", post(reset_password))
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
