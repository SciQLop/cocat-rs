//! What the page knows about its room and its visitor, read from the meta tags the server
//! put in its head.

use std::str::FromStr;

use cocat_core::role::{Role, UnknownRole};
use thiserror::Error;
use url::Url;
use url::form_urlencoded::byte_serialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomConfig {
    pub room_id: String,
    pub room_name: String,
    pub role: Role,
    /// Absolute, `ws:` or `wss:`.
    pub ws_url: String,
    pub api_url: String,
    pub user_email: String,
    /// Whether the visitor may open the admin pages.
    pub admin: bool,
    /// The project holds a SciQLop workspace's files besides its catalogues.
    pub files: bool,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("the page has no {0} meta tag")]
    Missing(&'static str),
    #[error(transparent)]
    Role(#[from] UnknownRole),
    #[error("{0} is not a URL this page can resolve")]
    Url(String),
}

impl RoomConfig {
    /// Reads the tags through `meta` (a tag's name to its content) and resolves the URLs
    /// against the page's base URI.
    pub fn from_meta(
        meta: impl Fn(&str) -> Option<String>,
        base_uri: &str,
    ) -> Result<Self, ConfigError> {
        let tag = |name: &'static str| meta(name).ok_or(ConfigError::Missing(name));
        Ok(Self {
            room_id: tag("cocat-room")?,
            room_name: tag("cocat-room-name")?,
            role: Role::from_str(&tag("cocat-role")?)?,
            ws_url: socket_url(&tag("cocat-ws")?, base_uri)?,
            api_url: resolve(&tag("cocat-api")?, base_uri)?.into(),
            user_email: tag("cocat-user")?,
            admin: meta("cocat-admin").is_some_and(|value| value == "true"),
            files: meta("cocat-files").is_some_and(|value| value == "true"),
        })
    }
}

/// The sign-in page, asked to come back to `here` (a path on this site) afterwards.
pub fn login_url(base_uri: &str, here: &str) -> Result<String, ConfigError> {
    let next: String = byte_serialize(here.as_bytes()).collect();
    Ok(resolve(&format!("../login?next={next}"), base_uri)?.into())
}

/// The server may name the socket by its path only (it does not guess its own public
/// origin), and `new WebSocket` takes a relative URL only in recent browsers.
fn socket_url(meta: &str, base_uri: &str) -> Result<String, ConfigError> {
    let mut url = resolve(meta, base_uri)?;
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        other => other,
    }
    .to_owned();
    url.set_scheme(&scheme)
        .map_err(|()| ConfigError::Url(meta.to_owned()))?;
    Ok(url.into())
}

fn resolve(reference: &str, base_uri: &str) -> Result<Url, ConfigError> {
    Url::parse(base_uri)
        .and_then(|base| base.join(reference))
        .map_err(|_| ConfigError::Url(reference.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use cocat_core::role::Role;

    use super::*;

    const BASE: &str = "https://cocat.example.org/cocat-dev/ui/app/";

    fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn full(socket: &str) -> HashMap<String, String> {
        tags(&[
            ("cocat-room", "lab"),
            ("cocat-room-name", "Solitary Waves"),
            ("cocat-role", "viewer"),
            ("cocat-ws", socket),
            ("cocat-api", "/cocat-dev/api"),
            ("cocat-user", "alice@example.org"),
        ])
    }

    fn read(meta: &HashMap<String, String>, base: &str) -> Result<RoomConfig, ConfigError> {
        RoomConfig::from_meta(|name| meta.get(name).cloned(), base)
    }

    #[test]
    fn a_config_reads_the_meta_tags() {
        let config = read(&full("/cocat-dev/room/lab"), BASE).unwrap();
        assert_eq!(
            config,
            RoomConfig {
                room_id: "lab".into(),
                room_name: "Solitary Waves".into(),
                role: Role::Viewer,
                ws_url: "wss://cocat.example.org/cocat-dev/room/lab".into(),
                api_url: "https://cocat.example.org/cocat-dev/api".into(),
                user_email: "alice@example.org".into(),
                admin: false,
                files: false,
            }
        );
    }

    #[test]
    fn an_admins_page_says_so() {
        let mut meta = full("/room/lab");
        meta.insert("cocat-admin".into(), "true".into());
        assert!(read(&meta, BASE).unwrap().admin);
    }

    #[test]
    fn a_page_without_files_says_so() {
        assert!(!read(&full("/room/lab"), BASE).unwrap().files);
    }

    #[test]
    fn a_page_with_files_says_so() {
        let mut meta = full("/room/lab");
        meta.insert("cocat-files".into(), "true".into());
        assert!(read(&meta, BASE).unwrap().files);
    }

    #[test]
    fn a_plain_http_page_opens_a_plain_socket() {
        let config = read(&full("/room/lab"), "http://127.0.0.1:18131/ui/app/").unwrap();
        assert_eq!(config.ws_url, "ws://127.0.0.1:18131/room/lab");
    }

    #[test]
    fn an_absolute_socket_url_is_kept() {
        let config = read(&full("wss://elsewhere.org/room/lab"), BASE).unwrap();
        assert_eq!(config.ws_url, "wss://elsewhere.org/room/lab");
    }

    #[test]
    fn a_missing_tag_is_named() {
        let mut meta = full("/room/lab");
        meta.remove("cocat-ws");
        assert!(matches!(
            read(&meta, BASE),
            Err(ConfigError::Missing("cocat-ws"))
        ));
    }

    #[test]
    fn an_unknown_role_is_refused() {
        let mut meta = full("/room/lab");
        meta.insert("cocat-role".into(), "boss".into());
        assert!(matches!(read(&meta, BASE), Err(ConfigError::Role(_))));
    }

    #[test]
    fn signing_in_comes_back_to_this_page() {
        let login = login_url(BASE, "/cocat-dev/ui/rooms/lab/browse#c=1").unwrap();
        assert_eq!(
            login,
            "https://cocat.example.org/cocat-dev/ui/login?next=%2Fcocat-dev%2Fui%2Frooms%2Flab%2Fbrowse%23c%3D1"
        );
    }
}
