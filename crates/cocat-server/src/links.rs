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

    /// Absolute browser path of a page, reverse-proxy prefix included.
    ///
    /// A link is opened from someone else's browser, so its origin is not the `Host` header
    /// of the request that created it; a deployment behind a proxy sets `COCAT_PUBLIC_URL`
    /// and `COCAT_PROXY_PREFIX` instead.
    pub fn ui(&self, path_and_fragment: &str) -> String {
        format!("{}{}", self.origin, self.path(path_and_fragment))
    }

    /// The page that accepts a group invitation, whose token sits in the path.
    pub fn group_invitation(&self, token: &str) -> String {
        self.ui(&format!("/groups/join/{token}"))
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
