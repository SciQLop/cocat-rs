//! Browser-facing links, built from configuration rather than from the request.
//!
//! Both the mails and (later) the web UI hand out links, so the prefix arithmetic lives
//! here: `Links::ui` for an absolute path inside the app.

use thiserror::Error;

/// The origin others reach the app at, and the reverse-proxy prefix.
///
/// `Default` is the link set of a server with no configured origin: `ui` then answers a
/// relative path, which is what a test or a local run without a mailer needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Links {
    origin: String,
    prefix: String,
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
        })
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
    /// a path this server built already has it.
    pub fn prefixed(&self, path: &str) -> String {
        let carries = path == self.prefix
            || path
                .strip_prefix(&self.prefix)
                .is_some_and(|rest| rest.starts_with('/'));
        if self.prefix.is_empty() || carries {
            path.to_owned()
        } else {
            format!("{}{}", self.prefix, path)
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
    use super::Links;

    fn links(origin: &str, prefix: &str) -> Links {
        Links::from_env(&|name| match name {
            "COCAT_PUBLIC_URL" => Some(origin.to_owned()),
            "COCAT_PROXY_PREFIX" => Some(prefix.to_owned()),
            _ => None,
        })
        .unwrap()
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
