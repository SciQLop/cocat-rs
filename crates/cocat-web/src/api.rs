//! The project and history routes of the server's API. Same-origin requests, so the browser
//! sends the session cookie with each.

use gloo_net::http::{Request, RequestBuilder, Response};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use thiserror::Error;

use crate::history::{Diff, History, Version, endpoint, refusal_text, save_body};
use crate::projects::{Project, api_url};
use crate::session::RoomConfig;
use crate::sharing::Created;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("the server cannot be reached: {0}")]
    Network(#[from] gloo_net::Error),
    #[error("{0}")]
    Refused(String),
    #[error("the server's answer does not read: {0}")]
    Format(#[from] serde_json::Error),
    #[error("the server's address cannot be built")]
    Url,
}

/// The versions, newest first: of the room, or those touching a catalogue or an event.
pub async fn history(
    config: &RoomConfig,
    catalogue: Option<&str>,
    event: Option<&str>,
) -> Result<Vec<Version>, ApiError> {
    let query: Vec<(&str, &str)> = [("catalogue", catalogue), ("event", event)]
        .into_iter()
        .filter_map(|(key, value)| Some((key, value?)))
        .collect();
    let url = url(config, &[], &query)?;
    let history: History = json(Request::get(&url)).await?;
    Ok(history.versions)
}

/// What the version changed, for a catalogue or the room.
pub async fn changes(
    config: &RoomConfig,
    version: u64,
    catalogue: Option<&str>,
) -> Result<Diff, ApiError> {
    let query: Vec<(&str, &str)> = catalogue
        .map(|uuid| ("catalogue", uuid))
        .into_iter()
        .collect();
    let url = url(config, &[&version.to_string()], &query)?;
    json(Request::get(&url)).await
}

/// The room as of the version, as one Yjs update.
pub async fn state(config: &RoomConfig, version: u64) -> Result<Vec<u8>, ApiError> {
    let url = url(config, &[&version.to_string(), "state"], &[])?;
    let response = checked(Request::get(&url).send().await?).await?;
    Ok(response.binary().await?)
}

/// Makes the catalogue, or the room, equal to the version again; false when it already was.
pub async fn restore(
    config: &RoomConfig,
    version: u64,
    catalogue: Option<&str>,
) -> Result<bool, ApiError> {
    let url = url(config, &[&version.to_string(), "restore"], &[])?;
    let body = json!({ "catalogue": catalogue }).to_string();
    let restored: serde_json::Value = json_body(Request::post(&url), body).await?;
    Ok(restored["restored"].as_bool().unwrap_or(false))
}

/// Names the version; a blank name removes its name.
pub async fn name(config: &RoomConfig, version: u64, name: &str) -> Result<(), ApiError> {
    let url = url(config, &[&version.to_string(), "name"], &[])?;
    let request = match name.trim() {
        "" => Request::delete(&url).build()?,
        name => Request::put(&url)
            .header("Content-Type", "application/json")
            .body(json!({ "name": name }).to_string())?,
    };
    checked(request.send().await?).await?;
    Ok(())
}

/// Saves the newest version with a name and a note, each optional; its id comes back.
pub async fn save_version(config: &RoomConfig, name: &str, note: &str) -> Result<u64, ApiError> {
    #[derive(serde::Deserialize)]
    struct Saved {
        version: u64,
    }
    let url = url(config, &["commit"], &[])?;
    let saved: Saved = json_body(Request::post(&url), save_body(name, note).to_string()).await?;
    Ok(saved.version)
}

/// Makes a new project, owned by the visitor, holding the version; its id comes back.
pub async fn copy_version(
    config: &RoomConfig,
    version: u64,
    name: &str,
) -> Result<String, ApiError> {
    #[derive(serde::Deserialize)]
    struct Copied {
        room: String,
    }
    let url = url(config, &[&version.to_string(), "copy"], &[])?;
    let body = json!({ "name": name }).to_string();
    let copied: Copied = json_body(Request::post(&url), body).await?;
    Ok(copied.room)
}

/// The visitor's projects, by name.
pub async fn projects(api: &str) -> Result<Vec<Project>, ApiError> {
    json(Request::get(&at(api, &["rooms"])?)).await
}

/// A new project, owned by the visitor.
pub async fn create_project(api: &str, name: &str) -> Result<Project, ApiError> {
    let body = json!({ "name": name }).to_string();
    json_body(Request::post(&at(api, &["rooms"])?), body).await
}

pub async fn rename_project(api: &str, id: &str, name: &str) -> Result<Project, ApiError> {
    let body = json!({ "name": name }).to_string();
    json_body(Request::patch(&at(api, &["rooms", id])?), body).await
}

pub async fn delete_project(api: &str, id: &str) -> Result<(), ApiError> {
    send(Request::delete(&at(api, &["rooms", id])?)).await
}

/// Leaving is removing one's own membership, which takes the visitor's account id.
pub async fn leave_project(api: &str, id: &str) -> Result<(), ApiError> {
    let me = me(api).await?;
    send(Request::delete(&at(api, &["rooms", id, "members", &me])?)).await
}

async fn me(api: &str) -> Result<String, ApiError> {
    #[derive(serde::Deserialize)]
    struct Me {
        id: String,
    }
    let me: Me = json(Request::get(&at(api, &["me"])?)).await?;
    Ok(me.id)
}

/// A GET of `<api>/<segments…>`.
pub async fn read<T: DeserializeOwned>(api: &str, segments: &[&str]) -> Result<T, ApiError> {
    json(Request::get(&at(api, segments)?)).await
}

/// A DELETE of `<api>/<segments…>`.
pub async fn remove(api: &str, segments: &[&str]) -> Result<(), ApiError> {
    send(Request::delete(&at(api, segments)?)).await
}

pub async fn patch(api: &str, segments: &[&str], body: Value) -> Result<(), ApiError> {
    write(Request::patch, api, segments, body).await
}

pub async fn put(api: &str, segments: &[&str], body: Value) -> Result<(), ApiError> {
    write(Request::put, api, segments, body).await
}

/// A POST whose answer is not needed.
pub async fn post(api: &str, segments: &[&str], body: Value) -> Result<(), ApiError> {
    write(Request::post, api, segments, body).await
}

/// A POST that makes something, and what it made.
pub async fn create<T: DeserializeOwned>(
    api: &str,
    segments: &[&str],
    body: Value,
) -> Result<T, ApiError> {
    json_body(Request::post(&at(api, segments)?), body.to_string()).await
}

/// A request with a JSON body whose answer is not needed.
async fn write(
    request: fn(&str) -> RequestBuilder,
    api: &str,
    segments: &[&str],
    body: Value,
) -> Result<(), ApiError> {
    let request = request(&at(api, segments)?)
        .header("Content-Type", "application/json")
        .body(body.to_string())?;
    checked(request.send().await?).await?;
    Ok(())
}

/// Invites to a project (`role` given) or a group (none): an address, or anyone with the
/// link when `email` is blank.
pub async fn invite(
    api: &str,
    segments: &[&str],
    email: &str,
    role: Option<&str>,
) -> Result<Created, ApiError> {
    let email = Some(email.trim()).filter(|email| !email.is_empty());
    let body = match role {
        Some(role) => json!({ "email": email, "role": role }),
        None => json!({ "email": email }),
    };
    create(api, segments, body).await
}

/// Removes the visitor from a group.
pub async fn leave_group(api: &str, id: &str) -> Result<(), ApiError> {
    let me = me(api).await?;
    remove(api, &["groups", id, "members", &me]).await
}

fn at(api: &str, segments: &[&str]) -> Result<String, ApiError> {
    api_url(api, segments).ok_or(ApiError::Url)
}

async fn send(request: RequestBuilder) -> Result<(), ApiError> {
    checked(request.send().await?).await?;
    Ok(())
}

fn url(config: &RoomConfig, path: &[&str], query: &[(&str, &str)]) -> Result<String, ApiError> {
    endpoint(&config.api_url, &config.room_id, path, query).ok_or(ApiError::Url)
}

async fn json<T: DeserializeOwned>(request: RequestBuilder) -> Result<T, ApiError> {
    let response = checked(request.send().await?).await?;
    Ok(serde_json::from_str(&response.text().await?)?)
}

async fn json_body<T: DeserializeOwned>(
    request: RequestBuilder,
    body: String,
) -> Result<T, ApiError> {
    let request = request
        .header("Content-Type", "application/json")
        .body(body)?;
    let response = checked(request.send().await?).await?;
    Ok(serde_json::from_str(&response.text().await?)?)
}

async fn checked(response: Response) -> Result<Response, ApiError> {
    if response.ok() {
        return Ok(response);
    }
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    Err(ApiError::Refused(refusal_text(status, &body)))
}
