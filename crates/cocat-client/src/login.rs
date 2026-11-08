//! Logging in to a cocat server and listing the rooms a session may enter.

use cocat_core::role::Role;
use cookie::Cookie;
use serde::Deserialize;
use thiserror::Error;
use url::Url;

/// The cookie the server sets on a successful login and reads on every later request.
pub(crate) const COOKIE: &str = "fastapiusersauth";

/// What a room logs in with again when the server refuses its cookie.
///
/// Deliberately not `Debug`: a password must not reach a log line.
#[derive(Clone)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

impl Credentials {
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Credentials {
        Credentials {
            username: username.into(),
            password: password.into(),
        }
    }
}

/// One room of a user, with the role the server gives that user there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomInfo {
    pub id: String,
    pub name: String,
    pub role: Role,
    /// The user's own project, made with the account: the one to open by default.
    pub personal: bool,
}

#[derive(Debug, Error)]
pub enum LoginError {
    #[error(transparent)]
    Url(#[from] url::ParseError),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("the server answered {0}")]
    Refused(u16),
    #[error("the server accepted the login without setting a session cookie")]
    NoCookie,
    #[error("the server sent a role no client knows: {0}")]
    Role(String),
}

/// Posts the credentials and returns the session cookie's value.
pub async fn login(url: &str, user: &str, password: &str) -> Result<String, LoginError> {
    let response = reqwest::Client::new()
        .post(base(url)?.join("auth/jwt/login")?)
        .form(&[("username", user), ("password", password)])
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(LoginError::Refused(response.status().as_u16()));
    }
    for header in response.headers().get_all(reqwest::header::SET_COOKIE) {
        let Ok(text) = header.to_str() else {
            continue;
        };
        let Ok(cookie) = Cookie::parse_encoded(text.to_owned()) else {
            continue;
        };
        if cookie.name() == COOKIE {
            return Ok(cookie.value().to_owned());
        }
    }
    Err(LoginError::NoCookie)
}

/// Lists the rooms the session may enter, with the role it holds in each.
pub async fn rooms(url: &str, cookie: &str) -> Result<Vec<RoomInfo>, LoginError> {
    let response = reqwest::Client::new()
        .get(base(url)?.join("api/rooms")?)
        .header(reqwest::header::COOKIE, cookie_header(cookie))
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(LoginError::Refused(response.status().as_u16()));
    }
    let rows: Vec<RoomRow> = response.json().await?;
    rows.into_iter().map(room_info).collect()
}

/// The names Python cocat and this server gave an account's own project.
const PERSONAL_ROOM_NAMES: [&str; 2] = ["My catalogues", "Personal"];

fn room_info(row: RoomRow) -> Result<RoomInfo, LoginError> {
    let role: Role = row.role.parse().map_err(|_| LoginError::Role(row.role))?;
    // A Python cocat server does not send `personal`: its own project is the owned one with
    // the name it gives it.
    let personal = row
        .personal
        .unwrap_or_else(|| role == Role::Owner && PERSONAL_ROOM_NAMES.contains(&row.name.as_str()));
    Ok(RoomInfo {
        id: row.id,
        name: row.name,
        role,
        personal,
    })
}

pub(crate) fn cookie_header(cookie: &str) -> String {
    Cookie::new(COOKIE, cookie).encoded().to_string()
}

#[derive(Deserialize)]
struct RoomRow {
    id: String,
    name: String,
    role: String,
    personal: Option<bool>,
}

/// The server url as a directory, so joining a path keeps any prefix the url carries.
pub(crate) fn base(url: &str) -> Result<Url, LoginError> {
    let mut url = Url::parse(url)?;
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::{RoomRow, room_info};

    fn listed(json: &str) -> bool {
        let row: RoomRow = serde_json::from_str(json).unwrap();
        room_info(row).unwrap().personal
    }

    #[test]
    fn the_servers_flag_wins_over_the_name() {
        assert!(listed(
            r#"{"id": "a", "name": "Mine", "role": "owner", "personal": true}"#
        ));
        assert!(!listed(
            r#"{"id": "a", "name": "Personal", "role": "owner", "personal": false}"#
        ));
    }

    #[test]
    fn without_the_flag_an_owned_room_named_like_an_own_project_is_one() {
        assert!(listed(
            r#"{"id": "a", "name": "Personal", "role": "owner"}"#
        ));
        assert!(listed(
            r#"{"id": "a", "name": "My catalogues", "role": "owner"}"#
        ));
        assert!(!listed(
            r#"{"id": "a", "name": "Personal", "role": "editor"}"#
        ));
        assert!(!listed(r#"{"id": "a", "name": "Lab", "role": "owner"}"#));
    }
}
