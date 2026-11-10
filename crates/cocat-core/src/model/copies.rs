//! The chain of copies a catalogue descends from, oldest first: the catalogue a copy was
//! made from is the last hop. It is stored under `copied_from` as an array of maps, one per
//! hop, and read defensively: whatever does not parse is skipped.

use serde::{Deserialize, Serialize};
use yrs::types::ToJson;
use yrs::{Any, Array, ArrayPrelim, In, Map as _, MapPrelim, MapRef, Out, ReadTxn, TransactionMut};

use super::CATALOGUES;
use super::read::object;
use super::values::typed;

/// The key a catalogue keeps its chain under.
pub(super) const KEY: &str = "copied_from";
const MAX_HOPS: usize = 32;

/// One copy: where it was made from, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Hop {
    pub project: String,
    pub project_name: String,
    pub catalogue: String,
    pub catalogue_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
    /// Milliseconds since the epoch.
    pub at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

/// The chain of `catalogue`; empty when it holds none or the room lacks the catalogue.
pub(super) fn chain_of<T: ReadTxn>(txn: &T, catalogue: &str) -> Vec<Hop> {
    object(txn, CATALOGUES, catalogue).map_or_else(Vec::new, |object| chain_in(txn, &object))
}

pub(super) fn chain_in<T: ReadTxn>(txn: &T, catalogue: &MapRef) -> Vec<Hop> {
    match catalogue.get(txn, KEY) {
        Some(Out::YArray(hops)) => hops
            .iter(txn)
            .filter_map(|hop| typed::<Hop>(&hop.to_json(txn)))
            .collect(),
        _ => Vec::new(),
    }
}

/// The chain a copy of `source` starts with: the source's own, then `hop`.
// simplify: beyond 32 hops the middle ones are dropped, keeping the original and the latest
// 31; raise the cap if chains ever get that long in practice.
pub fn copy_chain<T: ReadTxn>(txn: &T, source: &str, hop: Hop) -> Vec<Hop> {
    let mut chain = chain_of(txn, source);
    chain.push(hop);
    if chain.len() > MAX_HOPS {
        chain.drain(1..chain.len() - (MAX_HOPS - 1));
    }
    chain
}

/// Sets the chain of `catalogue`, or removes it when empty; nothing happens when the room
/// holds no such catalogue.
pub fn set_copied_from(txn: &mut TransactionMut<'_>, catalogue: &str, chain: &[Hop]) {
    let Some(object) = object(txn, CATALOGUES, catalogue) else {
        return;
    };
    if chain.is_empty() {
        object.remove(txn, KEY);
    } else {
        object.insert(txn, KEY, ArrayPrelim::from_iter(chain.iter().map(hop_map)));
    }
}

fn hop_map(hop: &Hop) -> MapPrelim {
    let version = hop.version.map(|version| Any::from(version as f64));
    let by = hop.by.as_deref().map(Any::from);
    MapPrelim::from_iter(
        [
            ("project", Some(Any::from(hop.project.as_str()))),
            ("project_name", Some(Any::from(hop.project_name.as_str()))),
            ("catalogue", Some(Any::from(hop.catalogue.as_str()))),
            (
                "catalogue_name",
                Some(Any::from(hop.catalogue_name.as_str())),
            ),
            ("version", version),
            ("at", Some(Any::from(hop.at))),
            ("by", by),
        ]
        .into_iter()
        .filter_map(|(key, value)| Some((key.to_owned(), In::Any(value?)))),
    )
}
