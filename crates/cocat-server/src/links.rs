//! Browser-facing links, built from configuration rather than from the request.
//!
//! Both the mails and (later) the web UI hand out links, so the prefix arithmetic lives
//! here: `Links::ui` for an absolute path inside the app.

use std::sync::{Arc, RwLock};

use thiserror::Error;

pub use cocat_core::room_id::path_segment as segment;

/// The origin others reach the app at, and the reverse-proxy prefix.
///
/// `Default` is the link set of a server with no configured origin: `ui` then answers a
/// relative path, which is what a test or a local run without a mailer needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Links {
    origin: String,
    prefix: String,
    /// Prefixes the server was reached under before, which `prefixed` swaps for `prefix`.
    aliases: Vec<String>,
    /// What every page tells visitors in a banner, such as a coming move.
    notice: Notice,
}

/// The banner text, set by an administrator and kept in the database. Every page builds its
/// links from the server's one `Links`, so the clones share this cell: a change shows on the
/// next page any visitor loads.
#[derive(Debug, Clone, Default)]
struct Notice(Arc<RwLock<Option<String>>>);

impl PartialEq for Notice {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || *read(&self.0) == *read(&other.0)
    }
}

impl Eq for Notice {}

/// A panic while the text was replaced leaves a whole `Option`: still readable.
fn read(cell: &RwLock<Option<String>>) -> std::sync::RwLockReadGuard<'_, Option<String>> {
    cell.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug, Error)]
pub enum LinksError {
    #[error(
        "COCAT_PUBLIC_URL must be an absolute URL such as https://cocat.example.org, not {0:?}"
    )]
    PublicUrl(String),
}

impl Links {
    pub fn from_env(env: &impl Fn(&str) -> Option<String>) -> Result<Links, LinksError> {
        Ok(Links {
            origin: origin(&env("COCAT_PUBLIC_URL").unwrap_or_default())?,
            prefix: prefix(&env("COCAT_PROXY_PREFIX").unwrap_or_default()),
            aliases: aliases(&env("COCAT_PROXY_PREFIX_ALIASES").unwrap_or_default()),
            notice: Notice::default(),
        })
    }

    /// The banner text every page shows, when an administrator set one.
    pub fn notice(&self) -> Option<String> {
        read(&self.notice.0).clone()
    }

    /// Shows `text` above every page from now on, or no banner with `None`.
    pub fn set_notice(&self, text: Option<String>) {
        *self
            .notice
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = text;
    }

    /// Browser path of a page, reverse-proxy prefix included and no origin.
    ///
    /// The UI links and redirects to its own pages from the request it is answering, so
    /// they carry the prefix but not `COCAT_PUBLIC_URL`.
    pub fn path(&self, path_and_fragment: &str) -> String {
        format!("{}/ui{}", self.prefix, path_and_fragment)
    }

    /// Path of a route outside `/ui`, reverse-proxy prefix included and no origin.
    ///
    /// The documentation pages are not part of the UI, so they build their paths through
    /// this instead of `path`.
    pub fn app(&self, path_and_fragment: &str) -> String {
        format!("{}{}", self.prefix, path_and_fragment)
    }

    /// `path` with the reverse-proxy prefix, unless it already carries it.
    ///
    /// The prefix is stripped by the proxy before the request reaches the server, so a
    /// path taken from a request or from a `next` parameter names the route without it;
    /// a path this server built already has it. A path under an old prefix, from a page
    /// opened before the prefix moved, gets the current one in its place.
    pub fn prefixed(&self, path: &str) -> String {
        let moved = self
            .aliases
            .iter()
            .find_map(|alias| under(path, alias))
            .filter(|rest| !rest.starts_with("//"));
        match moved {
            Some("") => format!("{}/", self.prefix),
            Some(rest) => format!("{}{rest}", self.prefix),
            None if under(path, &self.prefix).is_some() => path.to_owned(),
            None => format!("{}{path}", self.prefix),
        }
    }

    /// Absolute browser path of a page, reverse-proxy prefix included.
    ///
    /// A link is opened from someone else's browser, so its origin is not the `Host` header
    /// of the request that created it; a deployment behind a proxy sets `COCAT_PUBLIC_URL`
    /// and `COCAT_PROXY_PREFIX` instead.
    pub fn ui(&self, path_and_fragment: &str) -> String {
        format!("{}{}", self.origin, self.path(path_and_fragment))
    }

    /// The websocket URL of a route outside `/ui`, such as a room's socket.
    ///
    /// With `COCAT_PUBLIC_URL` it is absolute, `wss` for an `https` origin. Without, it is a
    /// path the browser resolves against the page it is on, so it keeps the scheme and host
    /// the browser really used even when a proxy rewrites the `Host` header or drops
    /// `X-Forwarded-Proto` on the way in.
    pub fn socket(&self, path: &str) -> String {
        let origin = match self.origin.split_once("://") {
            Some(("https", authority)) => format!("wss://{authority}"),
            Some(("http", authority)) => format!("ws://{authority}"),
            _ => self.origin.clone(),
        };
        format!("{origin}{}", self.app(path))
    }

    /// The address SciQLop and Python connect to, when `COCAT_PUBLIC_URL` names it.
    pub fn public_base(&self) -> Option<String> {
        (!self.origin.is_empty()).then(|| format!("{}{}", self.origin, self.prefix))
    }

    /// The page that accepts a group invitation, whose token sits in the fragment so it
    /// never reaches an access log or a Referer header.
    pub fn group_invitation(&self, token: &str) -> String {
        self.ui(&format!("/groups/join#{token}"))
    }
}

/// `COCAT_PUBLIC_URL` without its path, query and fragment.
///
/// A path in that variable would repeat the prefix that `prefix` already adds, so only the
/// origin is kept, as Python's `urls.py::public_base` does.
fn origin(configured: &str) -> Result<String, LinksError> {
    let configured = configured.trim();
    if configured.is_empty() {
        return Ok(String::new());
    }
    let Some((scheme, rest)) = configured.split_once("://") else {
        return Err(LinksError::PublicUrl(configured.to_string()));
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if scheme.is_empty() || authority.is_empty() {
        return Err(LinksError::PublicUrl(configured.to_string()));
    }
    Ok(format!("{scheme}://{authority}"))
}

/// The rest of `path` after `prefix`, when `path` is `prefix` itself or a path below it.
fn under<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    path.strip_prefix(prefix)
        .filter(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// `COCAT_PROXY_PREFIX_ALIASES`, a comma-separated list, each entry normalised like the prefix.
fn aliases(configured: &str) -> Vec<String> {
    configured
        .split(',')
        .map(prefix)
        .filter(|alias| !alias.is_empty())
        .collect()
}

/// `COCAT_PROXY_PREFIX` as `/prefix`, or empty, as Python's `prefix.py` normalises it.
fn prefix(configured: &str) -> String {
    let stripped = configured.trim().trim_matches('/');
    if stripped.is_empty() {
        String::new()
    } else {
        format!("/{stripped}")
    }
}

#[cfg(test)]
mod tests {
    use super::{Links, segment};

    #[test]
    fn an_imported_id_is_one_path_segment() {
        assert_eq!(segment("Bepi MSA?#%"), "Bepi%20MSA%3F%23%25");
        assert_eq!(segment("a/b.c+d~"), "a%2Fb.c+d~");
        assert_eq!(segment("lab-2_x"), "lab-2_x");
    }

    fn links(origin: &str, prefix: &str) -> Links {
        Links::from_env(&|name| match name {
            "COCAT_PUBLIC_URL" => Some(origin.to_owned()),
            "COCAT_PROXY_PREFIX" => Some(prefix.to_owned()),
            _ => None,
        })
        .unwrap()
    }

    #[test]
    fn the_public_base_is_the_origin_and_prefix() {
        assert_eq!(
            links("https://cocat.example.org/ignored", "cocat-dev").public_base(),
            Some("https://cocat.example.org/cocat-dev".to_owned())
        );
        assert_eq!(links("", "cocat-dev").public_base(), None);
    }

    fn moved(prefix: &str, aliases: &str) -> Links {
        Links::from_env(&|name| match name {
            "COCAT_PROXY_PREFIX" => Some(prefix.to_owned()),
            "COCAT_PROXY_PREFIX_ALIASES" => Some(aliases.to_owned()),
            _ => None,
        })
        .unwrap()
    }

    #[test]
    fn an_old_prefix_is_swapped_for_the_current_one() {
        let links = moved("cocat", " /cocat-dev/ , old,,");
        assert_eq!(links.prefixed("/cocat-dev/ui/x?a=1#b"), "/cocat/ui/x?a=1#b");
        assert_eq!(links.prefixed("/old/ui/x"), "/cocat/ui/x");
        assert_eq!(links.prefixed("/cocat-dev"), "/cocat/");
    }

    #[test]
    fn a_path_without_a_prefix_gets_it_once() {
        let links = moved("cocat", "cocat-dev");
        assert_eq!(links.prefixed("/ui/x"), "/cocat/ui/x");
        assert_eq!(links.prefixed("/cocat/ui/x"), "/cocat/ui/x");
    }

    #[test]
    fn a_longer_name_is_not_an_alias() {
        let links = moved("cocat", "cocat-dev");
        assert_eq!(links.prefixed("/cocat-devx/ui"), "/cocat/cocat-devx/ui");
    }

    #[test]
    fn swapping_to_no_prefix_never_leaves_a_host_path() {
        let links = moved("", "cocat-dev");
        assert_eq!(links.prefixed("/cocat-dev/ui/x"), "/ui/x");
        assert_eq!(links.prefixed("/cocat-dev"), "/");
        assert_eq!(
            links.prefixed("/cocat-dev//evil.com"),
            "/cocat-dev//evil.com"
        );
    }

    #[test]
    fn a_socket_url_follows_the_public_scheme_and_prefix() {
        assert_eq!(
            links("https://cocat.example.org/ignored", "cocat-dev").socket("/room/lab"),
            "wss://cocat.example.org/cocat-dev/room/lab"
        );
        assert_eq!(
            links("http://127.0.0.1:6570", "").socket("/room/lab"),
            "ws://127.0.0.1:6570/room/lab"
        );
        assert_eq!(links("", "/p/").socket("/room/lab"), "/p/room/lab");
    }
}
