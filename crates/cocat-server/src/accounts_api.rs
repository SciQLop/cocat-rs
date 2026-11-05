//! The account routes fastapi-users serves at the root: `POST /auth/register`. Logout lives
//! in `auth` because it is the cookie's business.

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use serde::Deserialize;

use crate::api::{ApiError, JsonBody, UserRead};
use crate::mail::{self, Address};
use crate::{AppState, accounts, db};

pub fn router() -> Router<AppState> {
    Router::new().route("/auth/register", post(register))
}

#[derive(Debug, Deserialize)]
pub(crate) struct RegisterBody {
    email: RegisterEmail,
    password: String,
}

/// Python's `UserCreate` refuses a reserved placeholder in a field validator, so the address
/// is settled while the body is deserialized: a malformed one and a placeholder are both the
/// same 422 as any other schema error, and neither creates a row.
#[derive(Debug, Deserialize)]
#[serde(try_from = "String")]
pub(crate) struct RegisterEmail(String);

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
pub(crate) async fn register(
    State(state): State<AppState>,
    JsonBody(body): JsonBody<RegisterBody>,
) -> Result<(StatusCode, Json<UserRead>), ApiError> {
    let id = accounts::register(&state, &body.email.0, &body.password).await?;
    let user = db::user_by_id(&state.pool, id)
        .await?
        .ok_or(ApiError::Internal)?;
    Ok((StatusCode::CREATED, Json(UserRead::from(user))))
}
