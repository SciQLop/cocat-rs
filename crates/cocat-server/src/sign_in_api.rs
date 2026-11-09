//! `/api/sign-in-requests`: how an app without a password signs in through the browser (see
//! `sign_in`). Neither route needs a session: the app has none yet.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::api::{ApiError, JsonBody};
use crate::links::Links;
use crate::openapi::Detail;
use crate::ratelimit::{self, LoginLimit};
use crate::sign_in::{self, Polled, SignInError};

const NO_NAME: &str = "Name the app that signs in, such as \"Python on my laptop\"";
const SLOW_DOWN: &str = "slow_down";

/// Opening a request is limited like a login, per client; polling is limited per request by
/// its interval instead.
pub fn router(limit: &LoginLimit) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(create))
        .layer(middleware::from_fn_with_state(
            limit.clone(),
            ratelimit::limit,
        ))
        .routes(routes!(poll))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct NewRequest {
    /// What the account page will call the app: "Python on lpp-jupyter".
    name: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct RequestOut {
    /// Shown by the app and on the page, for the person to compare: "K7QM-2XRD".
    code: String,
    /// The page the person opens to allow the app.
    url: String,
    /// The secret the app polls with. Never show it.
    poll: String,
    /// Seconds before the request expires.
    expires_in: u32,
    /// Seconds to wait between polls.
    interval: u32,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct PollBody {
    poll: String,
}

/// `pending`, `denied`, `expired` (also for an unknown or spent request), or `approved` with
/// the token, which is handed out once.
#[derive(Debug, Serialize, utoipa::ToSchema)]
struct PollOut {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,
}

#[utoipa::path(
    post,
    path = "/api/sign-in-requests",
    request_body = NewRequest,
    responses(
        (status = 201, description = "The request: show `url` and `code`, poll with `poll`", body = RequestOut),
        (status = 400, description = "The name is empty", body = Detail),
        (status = 422, description = "The body is not a name", body = Detail),
        (status = 429, description = "Too many requests", body = Detail),
    )
)]
async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<NewRequest>,
) -> Result<(StatusCode, Json<RequestOut>), ApiError> {
    let created = sign_in::create(&state.pool, &body.name)
        .await
        .map_err(api_error)?;
    let url = format!(
        "{}/ui/connect?code={}",
        base(&state.links, &headers),
        created.code
    );
    Ok((
        StatusCode::CREATED,
        Json(RequestOut {
            code: created.code,
            url,
            poll: created.poll,
            expires_in: sign_in::LIFETIME_SECONDS,
            interval: sign_in::POLL_INTERVAL_SECONDS,
        }),
    ))
}

#[utoipa::path(
    post,
    path = "/api/sign-in-requests/poll",
    request_body = PollBody,
    responses(
        (status = 200, description = "Where the request stands", body = PollOut),
        (status = 422, description = "The body has no poll id", body = Detail),
        (status = 429, description = "Polled again within the interval: `slow_down`", body = Detail),
    )
)]
async fn poll(
    State(state): State<AppState>,
    JsonBody(body): JsonBody<PollBody>,
) -> Result<Response, ApiError> {
    let polled = sign_in::poll(&state.pool, &body.poll)
        .await
        .map_err(api_error)?;
    let (status, token) = match polled {
        Polled::SlowDown => {
            let detail = serde_json::json!({ "detail": SLOW_DOWN });
            return Ok((StatusCode::TOO_MANY_REQUESTS, Json(detail)).into_response());
        }
        Polled::Pending => ("pending", None),
        Polled::Denied => ("denied", None),
        Polled::Expired => ("expired", None),
        Polled::Approved { token } => ("approved", Some(token)),
    };
    Ok(Json(PollOut { status, token }).into_response())
}

/// `COCAT_PUBLIC_URL` with the proxy prefix, or else the host the app called: the link is for
/// the person at that app, who reached the server at that address.
fn base(links: &Links, headers: &HeaderMap) -> String {
    if let Some(base) = links.public_base() {
        return base;
    }
    match headers
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
    {
        Some(host) => format!("http://{host}{}", links.app("")),
        None => links.app(""),
    }
}

fn api_error(error: SignInError) -> ApiError {
    match error {
        SignInError::NoName => ApiError::BadRequest(NO_NAME.to_owned()),
        other => {
            tracing::error!(error = %other, "a sign-in request failed");
            ApiError::Internal
        }
    }
}
