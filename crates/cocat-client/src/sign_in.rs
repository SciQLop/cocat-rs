//! Browser sign-in: the app asks the server for a request, shows its link and code, and
//! polls until the person allows or denies it in the browser. Allowed, it gets a token that
//! works wherever a session cookie does; [`sign_out`] ends it.

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::login::{LoginError, send};
use crate::projects::{api_request, api_url};

/// A request waiting for the person: the page to open, the code it shows, and the secret
/// the app polls with every `interval` seconds, for `expires_in` seconds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignInRequest {
    pub code: String,
    pub url: String,
    pub poll: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// Where a request stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum SignInStatus {
    Pending,
    Denied,
    /// Unknown, past its time, or already used: the app starts over.
    Expired,
    Approved {
        token: String,
    },
}

/// Asks for a request on behalf of the app called `name` ("Python on lab-laptop").
pub async fn start_sign_in(url: &str, name: &str) -> Result<SignInRequest, LoginError> {
    let request = reqwest::Client::new()
        .post(api_url(url, &["sign-in-requests"])?)
        .json(&json!({ "name": name }));
    Ok(send(request).await?.json().await?)
}

pub async fn poll_sign_in(url: &str, poll: &str) -> Result<SignInStatus, LoginError> {
    let request = reqwest::Client::new()
        .post(api_url(url, &["sign-in-requests", "poll"])?)
        .json(&json!({ "poll": poll }));
    match send(request).await {
        // `slow_down`: polled again within a second, so nothing changed yet.
        Err(LoginError::Refused { status: 429, .. }) => Ok(SignInStatus::Pending),
        answer => Ok(answer?.json().await?),
    }
}

/// Ends the session `token` carries, closing its sockets at once.
pub async fn sign_out(url: &str, token: &str) -> Result<(), LoginError> {
    send(api_request(
        Method::DELETE,
        url,
        token,
        &["tokens", "current"],
    )?)
    .await?;
    Ok(())
}
