//! The account's signed-in apps under `/api/tokens`: list them, revoke one, or revoke the
//! token the call itself carries (an app's logout).

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use serde::Serialize;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::{ApiError, ApiPath, CurrentUser};
use crate::db::{self, TokenRow};
use crate::openapi::Detail;
use crate::{AppState, auth};

const NO_SUCH_TOKEN: &str = "No such signed-in app";

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list))
        .routes(routes!(revoke_current))
        .routes(routes!(revoke))
}

/// A signed-in app. The token itself is never shown again after it was handed out.
#[derive(Debug, Serialize, utoipa::ToSchema)]
struct TokenOut {
    /// Names the token in `DELETE /api/tokens/{id}`; it is not the token.
    id: String,
    /// What the app called itself when it asked to sign in: "Python on lpp-jupyter".
    name: String,
    created_at: String,
    /// Updated at most once an hour. Six months after it, the token stops working.
    last_used_at: String,
}

impl From<TokenRow> for TokenOut {
    fn from(row: TokenRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            created_at: row.created_at,
            last_used_at: row.last_used_at,
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/tokens",
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The caller's signed-in apps, newest first", body = [TokenOut]),
        (status = 401, description = "No valid session", body = Detail),
    )
)]
async fn list(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Vec<TokenOut>>, ApiError> {
    let rows = db::list_tokens(&state.pool, user.id).await?;
    Ok(Json(rows.into_iter().map(TokenOut::from).collect()))
}

/// Sign this app out: the token (or cookie) the call carries stops working.
#[utoipa::path(
    delete,
    path = "/api/tokens/current",
    security(("cookie" = [])),
    responses(
        (status = 204, description = "The session this call carried is ended"),
        (status = 401, description = "No valid session", body = Detail),
    )
)]
async fn revoke_current(
    State(state): State<AppState>,
    CurrentUser(_): CurrentUser,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let hash = auth::request_token_hash(&headers).ok_or(ApiError::Unauthorized)?;
    db::delete_session(&state.pool, &hash).await?;
    state.rooms.end_session(&hash);
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/api/tokens/{id}",
    params(("id" = String, Path, description = "The id `GET /api/tokens` lists")),
    security(("cookie" = [])),
    responses(
        (status = 204, description = "The app is signed out and its sockets closed"),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "The caller has no signed-in app with that id", body = Detail),
    )
)]
async fn revoke(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(id): ApiPath<String>,
) -> Result<StatusCode, ApiError> {
    auth::revoke_token(&state, user.id, &id)
        .await?
        .ok_or_else(|| ApiError::NotFound(NO_SUCH_TOKEN.to_owned()))?;
    Ok(StatusCode::NO_CONTENT)
}
