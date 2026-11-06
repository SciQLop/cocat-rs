//! Logging in to a cocat server and listing the rooms a session may enter.

use cocat_core::role::Role;
use cookie::Cookie;
use serde::Deserialize;
use thiserror::Error;
use url::Url;

/// The cookie the server sets on a successful login and reads on every later request.
pub(crate) const COOKIE: &str = "fastapiusersauth";

/// One room of a user, with the role the server gives that user there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomInfo {
    pub id: String,
    pub name: String,
    pub role: Role,
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
    let mut rooms = Vec::with_capacity(rows.len());
    for row in rows {
        let role = row
            .role
            .parse()
            .map_err(|_| LoginError::Role(row.role.clone()))?;
        rooms.push(RoomInfo {
            id: row.id,
            name: row.name,
            role,
        });
    }
    Ok(rooms)
}

pub(crate) fn cookie_header(cookie: &str) -> String {
    Cookie::new(COOKIE, cookie).encoded().to_string()
}

#[derive(Deserialize)]
struct RoomRow {
    id: String,
    name: String,
    role: String,
}

/// The server url as a directory, so joining a path keeps any prefix the url carries.
fn base(url: &str) -> Result<Url, LoginError> {
    let mut url = Url::parse(url)?;
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}
