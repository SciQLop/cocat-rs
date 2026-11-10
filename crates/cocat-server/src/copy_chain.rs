//! Recording, in a copy made by the server, where each catalogue in it was copied from.

use cocat_core::model::{self, CatalogueSummary, Hop};
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

/// Appends to the chain of every catalogue in `doc`, the trash included, the hop of its
/// having been copied from `source` now. Meant for a document that is about to be written
/// as the copy, so the copy never exists without its chain.
pub(crate) fn record_copy(doc: &Doc, source: &Source) {
    let at = now_ms();
    rewrite_chains(doc, |txn, found| {
        let hop = Hop {
            project: source.project.to_owned(),
            project_name: source.project_name.to_owned(),
            catalogue: found.uuid.clone(),
            catalogue_name: found.name.clone(),
            version: source.version,
            at,
            by: Some(source.by.to_owned()),
        };
        model::copy_chain(txn, &found.uuid, hop)
    });
}

/// Drops the email from every hop but the last, in every catalogue of `doc`: a public
/// catalogue names the author who submitted it and nobody who copied it before.
pub(crate) fn keep_only_last_author(doc: &Doc) {
    rewrite_chains(doc, |_, found| {
        let mut chain = found.copied_from.clone();
        let earlier = chain.len().saturating_sub(1);
        chain[..earlier].iter_mut().for_each(|hop| hop.by = None);
        chain
    });
}

fn rewrite_chains(doc: &Doc, chain: impl Fn(&yrs::Transaction<'_>, &CatalogueSummary) -> Vec<Hop>) {
    let chains: Vec<(String, Vec<Hop>)> = {
        let txn = doc.transact();
        model::all_catalogues(&txn)
            .iter()
            .map(|found| (found.uuid.clone(), chain(&txn, found)))
            .collect()
    };
    let mut txn = doc.transact_mut();
    for (uuid, chain) in chains {
        model::set_copied_from(&mut txn, &uuid, &chain);
    }
}
