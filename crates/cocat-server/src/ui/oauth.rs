//! Signing in through an OpenID Connect provider.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use cookie::{Cookie, SameSite};
use openidconnect::core::{CoreAuthenticationFlow, CoreGenderClaim};
use openidconnect::{
    AuthorizationCode, ClaimsVerificationError, CsrfToken, EmptyAdditionalClaims, Nonce,
    OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier, Scope, SubjectIdentifier,
    TokenResponse, UserInfoClaims,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::oidc::{self, Identity};
use crate::{AppState, auth, mail};

const COOKIE: &str = "cocat_oidc";
const AUDIENCE: &str = "cocat:oidc";
const LIFETIME_SECONDS: u64 = 600;

/// What the provider will need back when it returns: the PKCE verifier, the nonce it must echo
/// and the state that proves this browser started the sign-in. Signed, never encrypted, and
/// short-lived; the session secret does the signing.
#[derive(Debug, Serialize, Deserialize)]
struct Pending {
    provider: String,
    verifier: String,
    nonce: String,
    state: String,
    aud: Vec<String>,
    exp: i64,
}

#[derive(Debug, Error)]
enum ExchangeError {
    #[error("the provider returned no identity token")]
    NoIdToken,
    #[error("the provider refused the code: {0}")]
    Token(String),
    #[error("the identity token is not valid: {0}")]
    Claims(#[from] ClaimsVerificationError),
    #[error("the provider profile could not be read: {0}")]
    Profile(String),
    #[error("cannot build an HTTP client: {0}")]
    Http(String),
}

pub(crate) async fn login(
    Path(name): Path<String>,
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if !state
        .login_limit
        .hit("/ui/oauth/login", &headers, Some(peer))
    {
        return failed(&state, "too_many");
    }
    if headers
        .get("sec-fetch-site")
        .and_then(|site| site.to_str().ok())
        == Some("cross-site")
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(provider) = provider(&state, &name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let mut request = provider.client.authorize_url(
        CoreAuthenticationFlow::AuthorizationCode,
        CsrfToken::new_random,
        Nonce::new_random,
    );
    for scope in &provider.config.scopes {
        request = request.add_scope(Scope::new(scope.clone()));
    }
    let (url, csrf, nonce) = request.set_pkce_challenge(challenge).url();
    let pending = Pending {
        provider: name.clone(),
        verifier: verifier.secret().to_owned(),
        nonce: nonce.secret().to_owned(),
        state: csrf.secret().to_owned(),
        aud: vec![AUDIENCE.to_owned()],
        exp: expiry(),
    };
    let Ok(value) = auth::signed_claims(&pending, &state.secret) else {
        tracing::warn!("cannot sign the provider state");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let response = Redirect::to(url.as_str()).into_response();
    with_cookie(response, &notice_cookie(&state, &value, LIFETIME_SECONDS))
}

pub(crate) async fn callback(
    Path(name): Path<String>,
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some(pending) = pending(&state, &headers) else {
        return bad_state(&state);
    };
    if pending.provider != name {
        return bad_state(&state);
    }
    let Some(provider) = provider(&state, &name) else {
        return cleared(&state, StatusCode::NOT_FOUND.into_response());
    };
    if query.get("state") != Some(&pending.state) {
        return bad_state(&state);
    }
    let Some(code) = query.get("code") else {
        return cleared(&state, failed(&state, "provider"));
    };
    let identity = match exchange(provider, &pending, code).await {
        Ok(identity) => identity,
        Err(error) => {
            tracing::warn!(%error, "the provider could not complete a sign-in");
            return cleared(&state, failed(&state, "provider"));
        }
    };
    match oidc::sign_in(&state, &provider.config, identity).await {
        Ok(user) if !user.is_active => cleared(&state, failed(&state, "credentials")),
        Ok(user) => signed_in(&state, user).await,
        Err(oidc::SignInError::Exists) => cleared(&state, failed(&state, "exists")),
        Err(error) => {
            tracing::warn!(%error, "cannot sign a provider account in");
            cleared(&state, StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}

async fn exchange(
    provider: &oidc::Provider,
    pending: &Pending,
    code: &str,
) -> Result<Identity, ExchangeError> {
    let http = openidconnect::reqwest::ClientBuilder::new()
        .redirect(openidconnect::reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| ExchangeError::Http(error.to_string()))?;
    let token = provider
        .client
        .exchange_code(AuthorizationCode::new(code.to_owned()))
        .map_err(|error| ExchangeError::Token(error.to_string()))?
        .set_pkce_verifier(PkceCodeVerifier::new(pending.verifier.clone()))
        .request_async(&http)
        .await
        .map_err(|error| ExchangeError::Token(error.to_string()))?;
    let id_token = token.id_token().ok_or(ExchangeError::NoIdToken)?;
    let nonce = Nonce::new(pending.nonce.clone());
    let claims = id_token.claims(&provider.client.id_token_verifier(), &nonce)?;
    let subject = claims.subject().as_str().to_owned();
    let email = match claims.email() {
        Some(email) => Some(email.as_str().to_owned()),
        None => user_info_email(provider, &http, token.access_token().secret(), &subject).await?,
    };
    Ok(Identity { subject, email })
}

async fn user_info_email(
    provider: &oidc::Provider,
    http: &openidconnect::reqwest::Client,
    access_token: &str,
    subject: &str,
) -> Result<Option<String>, ExchangeError> {
    let claims: UserInfoClaims<EmptyAdditionalClaims, CoreGenderClaim> = provider
        .client
        .user_info(
            openidconnect::AccessToken::new(access_token.to_owned()),
            Some(SubjectIdentifier::new(subject.to_owned())),
        )
        .map_err(|error| ExchangeError::Profile(error.to_string()))?
        .request_async(http)
        .await
        .map_err(|error| ExchangeError::Profile(error.to_string()))?;
    Ok(claims.email().map(|email| email.as_str().to_owned()))
}

async fn signed_in(state: &AppState, user: crate::db::User) -> Response {
    let token = match auth::start_session(&state.pool, user.id).await {
        Ok(token) => token,
        Err(error) => {
            tracing::warn!(%error, "cannot start a session for a provider sign-in");
            return cleared(state, StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
    };
    let target = if mail::is_placeholder(&user.email) {
        state.links.path("/confirm-email")
    } else {
        state.links.path("/rooms")
    };
    let response = Redirect::to(&target).into_response();
    let response = with_cookie(response, &auth::set_cookie(&token));
    cleared(state, response)
}

fn provider<'a>(state: &'a AppState, name: &str) -> Option<&'a oidc::Provider> {
    state
        .oidc
        .iter()
        .find(|provider| provider.config.name == name)
}

fn pending(state: &AppState, headers: &HeaderMap) -> Option<Pending> {
    let value = session(headers)?;
    auth::read_claims::<Pending>(&value, &state.secret, AUDIENCE).ok()
}

fn session(headers: &HeaderMap) -> Option<String> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    Cookie::split_parse_encoded(cookies)
        .filter_map(Result::ok)
        .find(|cookie| cookie.name() == COOKIE)
        .map(|cookie| cookie.value().to_owned())
}

fn failed(state: &AppState, key: &str) -> Response {
    Redirect::to(&format!("{}?error={key}", state.links.path("/login"))).into_response()
}

fn bad_state(state: &AppState) -> Response {
    cleared(
        state,
        (StatusCode::BAD_REQUEST, "Bad state").into_response(),
    )
}

fn cleared(state: &AppState, response: Response) -> Response {
    with_cookie(response, &notice_cookie(state, "", 0))
}

fn notice_cookie(state: &AppState, value: &str, lifetime: u64) -> String {
    Cookie::build((COOKIE.to_owned(), value.to_owned()))
        .path(format!("{}oauth/", state.links.path("")))
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Lax)
        .max_age(cookie::time::Duration::seconds(lifetime as i64))
        .build()
        .encoded()
        .to_string()
}

fn with_cookie(mut response: Response, written: &str) -> Response {
    match HeaderValue::from_str(written) {
        Ok(value) => {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
        Err(error) => tracing::warn!(%error, "cannot write a cookie"),
    }
    response
}

fn expiry() -> i64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (now + Duration::from_secs(LIFETIME_SECONDS)).as_secs() as i64
}
