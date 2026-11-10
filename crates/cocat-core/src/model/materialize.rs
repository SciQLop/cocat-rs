//! Turning a smart catalogue's current result into a static, editable catalogue.

use yrs::TransactionMut;

use super::copies::{Hop, copy_chain, set_copied_from};
use super::members::{Member, catalogue_members, rule_of};
use super::provenance::create_origin;
use super::read::object;
use super::values::{get, text};
use super::write::{add_catalogue_events, create_catalogue, create_event};
use super::{CATALOGUES, EventRecord, NewCatalogue, Origin, OriginKind, WriteError};
use crate::time::Timestamp;

/// Creates the catalogue `into` listing the current members of catalogue `uuid`. Its events
/// are listed as they are; each derived interval becomes a new event whose origin is a
/// `rule` origin recording the rule. The copy's chain is the smart catalogue's, then
/// `copied`. Returns false when the room holds no catalogue `uuid`.
pub fn materialize(
    txn: &mut TransactionMut<'_>,
    uuid: &str,
    into: &NewCatalogue,
    copied: Hop,
) -> Result<bool, WriteError> {
    let Some(members) = catalogue_members(txn, uuid) else {
        return Ok(false);
    };
    let mut listed = Vec::new();
    let mut derived = Vec::new();
    for member in members? {
        match member {
            Member::Event { record, .. } => listed.push(record.uuid),
            Member::Derived { start, stop, .. } => derived.push((start, stop)),
        }
    }
    if !derived.is_empty() {
        listed.extend(create_derived(txn, uuid, into, &derived)?);
    }
    create_catalogue(txn, into)?;
    add_catalogue_events(txn, &into.uuid, &listed);
    let chain = copy_chain(txn, uuid, copied);
    set_copied_from(txn, &into.uuid, &chain);
    Ok(true)
}

/// The events made for the derived intervals, under one new origin.
fn create_derived(
    txn: &mut TransactionMut<'_>,
    uuid: &str,
    into: &NewCatalogue,
    derived: &[(Timestamp, Timestamp)],
) -> Result<Vec<String>, WriteError> {
    let origin = rule_origin(txn, uuid, into);
    create_origin(txn, &origin)?;
    derived
        .iter()
        .map(|(start, stop)| {
            let mut record = EventRecord::new(uuid::Uuid::new_v4().to_string(), *start, *stop);
            record.origin = Some(origin.id.clone());
            create_event(txn, &record)?;
            Ok(record.uuid)
        })
        .collect()
}

fn rule_origin(txn: &TransactionMut<'_>, uuid: &str, into: &NewCatalogue) -> Origin {
    let catalogue = object(txn, CATALOGUES, uuid);
    let name = catalogue
        .as_ref()
        .and_then(|map| get(map, txn, "name"))
        .as_ref()
        .and_then(text);
    Origin {
        id: uuid::Uuid::new_v4().to_string(),
        kind: OriginKind::Rule,
        name: name.unwrap_or_default(),
        version: None,
        author: into.author.clone(),
        created: into.created,
        used: Vec::new(),
        parameters: catalogue.as_ref().and_then(|map| rule_of(txn, map)),
        derived_from: Some(uuid.to_owned()),
    }
}
