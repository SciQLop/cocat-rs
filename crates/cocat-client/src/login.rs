//! Logging in to a cocat server and listing the rooms a session may enter.

use cocat_core::role::Role;
use cookie::Cookie;
use reqwest::{RequestBuilder, Response, StatusCode};
use serde::Deserialize;
use serde_json::Value;
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
    /// Whether the project's editors may share it as its owner does.
    pub editors_can_share: bool,
}

#[derive(Debug, Error)]
pub enum LoginError {
    #[error(transparent)]
    Url(#[from] url::ParseError),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    /// The server refused the call; `detail` is its own text (the JSON `detail` field).
    #[error("{detail}")]
    Refused { status: u16, detail: String },
    #[error("the server accepted the login without setting a session cookie")]
    NoCookie,
    #[error("the server sent a role no client knows: {0}")]
    Role(String),
}

/// Posts the credentials and returns the session cookie's value.
pub async fn login(url: &str, user: &str, password: &str) -> Result<String, LoginError> {
    let request = reqwest::Client::new()
        .post(base(url)?.join("auth/jwt/login")?)
        .form(&[("username", user), ("password", password)]);
    let response = send(request).await?;
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
    let request = authorized(
        reqwest::Client::new().get(base(url)?.join("api/rooms")?),
        cookie,
    );
    let rows: Vec<RoomRow> = send(request).await?.json().await?;
    rows.into_iter().map(room_info).collect()
}

/// The id of the account behind the session.
pub async fn account_id(url: &str, cookie: &str) -> Result<String, LoginError> {
    Ok(account(url, cookie).await?.id)
}

/// The account behind a session: its id, its address, and whether it curates the public
/// catalogues (a Python cocat server says nothing of curators: `false`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Account {
    pub id: String,
    pub email: String,
    #[serde(default)]
    pub is_curator: bool,
}

pub async fn account(url: &str, cookie: &str) -> Result<Account, LoginError> {
    let request = authorized(
        reqwest::Client::new().get(base(url)?.join("api/me")?),
        cookie,
    );
    Ok(send(request).await?.json().await?)
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
        editors_can_share: row.editors_can_share,
    })
}

/// Sends the request; an answer other than success becomes [`LoginError::Refused`].
pub(crate) async fn send(request: RequestBuilder) -> Result<Response, LoginError> {
    let response = request.send().await?;
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.bytes().await?;
    Err(LoginError::Refused {
        status: status.as_u16(),
        detail: detail(status, &body),
    })
}

/// The `detail` of a FastAPI-style error body: its text, or its JSON when structured (a 422
/// lists the fields at fault).
fn detail(status: StatusCode, body: &[u8]) -> String {
    #[derive(Deserialize)]
    struct ErrorBody {
        detail: Value,
    }
    match serde_json::from_slice::<ErrorBody>(body) {
        Ok(ErrorBody {
            detail: Value::String(text),
        }) => text,
        Ok(ErrorBody { detail }) => detail.to_string(),
        Err(_) => format!("the server answered {status}"),
    }
}

pub(crate) fn cookie_header(cookie: &str) -> String {
    Cookie::new(COOKIE, cookie).encoded().to_string()
}

/// The session sent both ways: this server reads the bearer token first, a Python cocat
/// server only the cookie. A token from a browser sign-in and a login cookie are the same kind
/// of secret here, so one value serves both.
pub(crate) fn authorized(request: RequestBuilder, session: &str) -> RequestBuilder {
    request
        .header(reqwest::header::COOKIE, cookie_header(session))
        .bearer_auth(session)
}

#[derive(Deserialize)]
struct RoomRow {
    id: String,
    name: String,
    role: String,
    personal: Option<bool>,
    /// Absent from a server that predates the setting, where only the owner shared.
    #[serde(default)]
    editors_can_share: bool,
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
    use super::{RoomRow, detail, room_info};
    use reqwest::StatusCode;

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

    #[test]
    fn the_detail_is_the_servers_text() {
        let body = br#"{"detail": "Nothing changed since the last saved version."}"#;
        assert_eq!(
            detail(StatusCode::CONFLICT, body),
            "Nothing changed since the last saved version."
        );
    }

    #[test]
    fn a_structured_detail_is_kept_as_json() {
        let body = br#"{"detail": [{"loc": ["body", "name"], "msg": "too long"}]}"#;
        assert_eq!(
            detail(StatusCode::UNPROCESSABLE_ENTITY, body),
            r#"[{"loc":["body","name"],"msg":"too long"}]"#
        );
    }

    #[test]
    fn without_a_detail_the_status_says_it() {
        assert_eq!(
            detail(StatusCode::BAD_GATEWAY, b"<html>proxy</html>"),
            "the server answered 502 Bad Gateway"
        );
    }
}
