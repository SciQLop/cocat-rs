//! The catalogues of other projects a project links to. They live in the `links` root of
//! the catalogues document, one map per link keyed by a fresh uuid, and are read
//! defensively: whatever does not parse is skipped. Nothing that exports reads this root.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use yrs::types::ToJson;
use yrs::{Any, In, Map as _, MapPrelim, ReadTxn, TransactionMut, WriteTxn};

use super::read::entries;
use super::values::typed;

pub const LINKS: &str = "links";

/// A link as stored: the catalogue of another project, named as it was when linked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Link {
    pub id: String,
    pub project: String,
    pub catalogue: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// Milliseconds since the epoch.
    pub at: i64,
}

/// What a caller supplies to add a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewLink {
    pub project: String,
    pub catalogue: String,
    pub name: String,
    pub by: Option<String>,
    pub at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum LinkError {
    #[error("that catalogue is already in this project")]
    SameProject,
    #[error("that catalogue is already linked")]
    AlreadyLinked,
}

/// What a link map holds: everything but its id, which is its key.
#[derive(Deserialize)]
struct Stored {
    project: String,
    catalogue: String,
    name: String,
    #[serde(default)]
    by: Option<String>,
    at: i64,
}

/// The links, sorted by name ignoring case, then by id.
pub fn links<T: ReadTxn>(txn: &T) -> Vec<Link> {
    let mut links: Vec<Link> = entries(txn, LINKS)
        .into_iter()
        .filter_map(|(id, map)| {
            let stored: Stored = typed(&map.to_json(txn))?;
            Some(Link {
                id,
                project: stored.project,
                catalogue: stored.catalogue,
                name: stored.name,
                by: stored.by,
                at: stored.at,
            })
        })
        .collect();
    links.sort_by_cached_key(|link| (link.name.to_lowercase(), link.id.clone()));
    links
}

/// Adds a link and returns its id; `this_project` is the project the document belongs to.
pub fn add_link(
    txn: &mut TransactionMut<'_>,
    this_project: &str,
    link: NewLink,
) -> Result<String, LinkError> {
    if link.project == this_project {
        return Err(LinkError::SameProject);
    }
    let known = links(txn)
        .iter()
        .any(|held| held.project == link.project && held.catalogue == link.catalogue);
    if known {
        return Err(LinkError::AlreadyLinked);
    }
    let id = uuid::Uuid::new_v4().to_string();
    txn.get_or_insert_map(LINKS)
        .insert(txn, id.as_str(), link_map(&link));
    Ok(id)
}

/// Removes a link; false when there was none by that id.
pub fn remove_link(txn: &mut TransactionMut<'_>, id: &str) -> bool {
    txn.get_or_insert_map(LINKS).remove(txn, id).is_some()
}

fn link_map(link: &NewLink) -> MapPrelim {
    let by = link.by.as_deref().map(Any::from);
    MapPrelim::from_iter(
        [
            ("project", Some(Any::from(link.project.as_str()))),
            ("catalogue", Some(Any::from(link.catalogue.as_str()))),
            ("name", Some(Any::from(link.name.as_str()))),
            ("by", by),
            ("at", Some(Any::from(link.at))),
        ]
        .into_iter()
        .filter_map(|(key, value)| Some((key.to_owned(), In::Any(value?)))),
    )
}
