//! Selection rounds: their settings, members, state, proposals and votes. Answers stay
//! JSON, shaped as the server sends them.

use reqwest::Method;
use serde_json::{Value, json};

use crate::login::{LoginError, send};
use crate::projects::api_request;

async fn call(
    method: Method,
    url: &str,
    cookie: &str,
    path: &[&str],
    body: Option<&Value>,
) -> Result<Value, LoginError> {
    let request = api_request(method, url, cookie, path)?;
    let request = match body {
        Some(body) => request.json(body),
        None => request,
    };
    Ok(send(request).await?.json().await?)
}

pub async fn rounds(url: &str, cookie: &str, project: &str) -> Result<Value, LoginError> {
    call(
        Method::GET,
        url,
        cookie,
        &["rooms", project, "rounds"],
        None,
    )
    .await
}

/// `body` holds `name`, `visibility` and optionally `deadline`, `budget`, `kind` and
/// `candidates` (`{project, catalogue}`).
pub async fn create_round(
    url: &str,
    cookie: &str,
    project: &str,
    body: &Value,
) -> Result<Value, LoginError> {
    let path = ["rooms", project, "rounds"];
    call(Method::POST, url, cookie, &path, Some(body)).await
}

pub async fn round(
    url: &str,
    cookie: &str,
    project: &str,
    round: &str,
) -> Result<Value, LoginError> {
    let path = ["rooms", project, "rounds", round];
    call(Method::GET, url, cookie, &path, None).await
}

/// `changes` holds only the settings to change; a `null` deadline or budget clears it.
pub async fn update_round(
    url: &str,
    cookie: &str,
    project: &str,
    round: &str,
    changes: &Value,
) -> Result<Value, LoginError> {
    let path = ["rooms", project, "rounds", round];
    call(Method::PATCH, url, cookie, &path, Some(changes)).await
}

/// Replaces the round's seats; each list holds emails.
pub async fn set_round_members(
    url: &str,
    cookie: &str,
    project: &str,
    round: &str,
    proposers: &[String],
    voters: &[String],
    deciders: &[String],
) -> Result<Value, LoginError> {
    let path = ["rooms", project, "rounds", round, "members"];
    let body = json!({"proposers": proposers, "voters": voters, "deciders": deciders});
    call(Method::PUT, url, cookie, &path, Some(&body)).await
}

pub async fn move_round(
    url: &str,
    cookie: &str,
    project: &str,
    round: &str,
    state: &str,
    final_catalogue: Option<&str>,
    note: Option<&str>,
) -> Result<Value, LoginError> {
    let path = ["rooms", project, "rounds", round, "state"];
    let body = json!({"state": state, "final_catalogue": final_catalogue, "note": note});
    call(Method::POST, url, cookie, &path, Some(&body)).await
}

/// The caller's proposal document, made on first use: `{doc}`.
pub async fn my_proposal(
    url: &str,
    cookie: &str,
    project: &str,
    round: &str,
) -> Result<Value, LoginError> {
    let path = ["rooms", project, "rounds", round, "proposal"];
    call(Method::POST, url, cookie, &path, None).await
}

/// Replaces the caller's votes; the server answers with nothing.
pub async fn set_votes(
    url: &str,
    cookie: &str,
    project: &str,
    round: &str,
    events: &[String],
) -> Result<(), LoginError> {
    let path = ["rooms", project, "rounds", round, "votes"];
    let request = api_request(Method::PUT, url, cookie, &path)?.json(&json!({"events": events}));
    send(request).await?;
    Ok(())
}

pub async fn votes(
    url: &str,
    cookie: &str,
    project: &str,
    round: &str,
) -> Result<Value, LoginError> {
    let path = ["rooms", project, "rounds", round, "votes"];
    call(Method::GET, url, cookie, &path, None).await
}

/// Sets or, with `None`, clears the caller's verdict (`accept`, `reject` or `unsure`) on one event.
pub async fn set_verdict(
    url: &str,
    cookie: &str,
    project: &str,
    round: &str,
    event: &str,
    verdict: Option<&str>,
) -> Result<(), LoginError> {
    let path = ["rooms", project, "rounds", round, "verdicts", event];
    let request = api_request(Method::PUT, url, cookie, &path)?.json(&json!({"verdict": verdict}));
    send(request).await?;
    Ok(())
}

/// Merges the caller's verdicts, `{event: "accept" | "reject" | "unsure" | null}`.
pub async fn set_verdicts(
    url: &str,
    cookie: &str,
    project: &str,
    round: &str,
    verdicts: &Value,
) -> Result<(), LoginError> {
    let path = ["rooms", project, "rounds", round, "verdicts"];
    let request =
        api_request(Method::PUT, url, cookie, &path)?.json(&json!({"verdicts": verdicts}));
    send(request).await?;
    Ok(())
}

/// `{revision, events: [{event, accept, reject, unsure, by?}]}`, as far as the caller may read.
pub async fn verdicts(
    url: &str,
    cookie: &str,
    project: &str,
    round: &str,
) -> Result<Value, LoginError> {
    let path = ["rooms", project, "rounds", round, "verdicts"];
    call(Method::GET, url, cookie, &path, None).await
}
