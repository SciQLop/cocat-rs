//! Recording, in a copy made by the server, where each catalogue in it was copied from.

use cocat_core::model::{self, Hop};
use yrs::{Doc, Transact};

use crate::room::now_ms;

/// The project a copy is made from and who copies it.
pub(crate) struct Source<'a> {
    pub project: &'a str,
    pub project_name: &'a str,
    /// The history version the copy holds, when the server knows it.
    pub version: Option<u64>,
    pub by: &'a str,
}

/// Appends to the chain of every catalogue in `doc` the hop of its having been copied from
/// `source` now. Meant for a document that is about to be written as the copy, so the copy
/// never exists without its chain.
pub(crate) fn record_copy(doc: &Doc, source: &Source) {
    let at = now_ms();
    let chains: Vec<(String, Vec<Hop>)> = {
        let txn = doc.transact();
        model::catalogues(&txn)
            .into_iter()
            .map(|found| {
                let hop = Hop {
                    project: source.project.to_owned(),
                    project_name: source.project_name.to_owned(),
                    catalogue: found.uuid.clone(),
                    catalogue_name: found.name,
                    version: source.version,
                    at,
                    by: Some(source.by.to_owned()),
                };
                let chain = model::copy_chain(&txn, &found.uuid, hop);
                (found.uuid, chain)
            })
            .collect()
    };
    let mut txn = doc.transact_mut();
    for (uuid, chain) in chains {
        model::set_copied_from(&mut txn, &uuid, &chain);
    }
}
