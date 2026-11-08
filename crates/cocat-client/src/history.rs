//! A room's version history, as the server keeps it: the versions, the room at one of them,
//! restoring one, naming one, copying one into a new room, and saving the newest.

use reqwest::{Method, RequestBuilder};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::login::{LoginError, send};
use crate::projects::room_request;

/// The versions, newest first, as the server describes them; with `catalogue`, only those
/// that touched it.
pub async fn history(
    url: &str,
    cookie: &str,
    room_id: &str,
    catalogue: Option<&str>,
) -> Result<Vec<Value>, LoginError> {
    #[derive(Deserialize)]
    struct History {
        versions: Vec<Value>,
    }
    let mut request = request(Method::GET, url, cookie, room_id, &[])?;
    if let Some(catalogue) = catalogue {
        request = request.query(&[("catalogue", catalogue)]);
    }
    let history: History = send(request).await?.json().await?;
    Ok(history.versions)
}

/// The room as of the version, as one update to apply to an empty document.
pub async fn version_state(
    url: &str,
    cookie: &str,
    room_id: &str,
    version: u64,
) -> Result<Vec<u8>, LoginError> {
    let version = version.to_string();
    let request = request(Method::GET, url, cookie, room_id, &[&version, "state"])?;
    Ok(send(request).await?.bytes().await?.to_vec())
}

/// Makes the room, or one catalogue, equal to the version again; false when it already was.
pub async fn restore(
    url: &str,
    cookie: &str,
    room_id: &str,
    version: u64,
    catalogue: Option<&str>,
) -> Result<bool, LoginError> {
    #[derive(Serialize)]
    struct Body<'a> {
        catalogue: Option<&'a str>,
    }
    #[derive(Deserialize)]
    struct Restored {
        restored: bool,
    }
    let version = version.to_string();
    let request = request(Method::POST, url, cookie, room_id, &[&version, "restore"])?
        .json(&Body { catalogue });
    let restored: Restored = send(request).await?.json().await?;
    Ok(restored.restored)
}

/// Names the version, or takes its name away with `None`.
pub async fn name_version(
    url: &str,
    cookie: &str,
    room_id: &str,
    version: u64,
    name: Option<&str>,
) -> Result<(), LoginError> {
    let version = version.to_string();
    let path = [version.as_str(), "name"];
    let request = match name {
        Some(name) => request(Method::PUT, url, cookie, room_id, &path)?
            .json(&serde_json::json!({ "name": name })),
        None => request(Method::DELETE, url, cookie, room_id, &path)?,
    };
    send(request).await?;
    Ok(())
}

/// Saves the room's newest version, ending it at the room's last change the server has; its
/// id comes back.
pub async fn commit(
    url: &str,
    cookie: &str,
    room_id: &str,
    name: Option<&str>,
    message: Option<&str>,
) -> Result<u64, LoginError> {
    #[derive(Serialize)]
    struct Body<'a> {
        name: Option<&'a str>,
        message: Option<&'a str>,
    }
    #[derive(Deserialize)]
    struct Committed {
        version: u64,
    }
    let request =
        request(Method::POST, url, cookie, room_id, &["commit"])?.json(&Body { name, message });
    let committed: Committed = send(request).await?.json().await?;
    Ok(committed.version)
}

/// Makes a new project, owned by the caller, holding the version; its id comes back.
pub async fn copy_version(
    url: &str,
    cookie: &str,
    room_id: &str,
    version: u64,
    name: &str,
) -> Result<String, LoginError> {
    #[derive(Deserialize)]
    struct Copied {
        room: String,
    }
    let version = version.to_string();
    let request = request(Method::POST, url, cookie, room_id, &[&version, "copy"])?
        .json(&serde_json::json!({ "name": name }));
    let copied: Copied = send(request).await?.json().await?;
    Ok(copied.room)
}

/// A request under `api/rooms/{room_id}/history`.
fn request(
    method: Method,
    url: &str,
    cookie: &str,
    room_id: &str,
    tail: &[&str],
) -> Result<RequestBuilder, LoginError> {
    let path: Vec<&str> = std::iter::once("history")
        .chain(tail.iter().copied())
        .collect();
    room_request(method, url, cookie, room_id, &path)
}
