//! Making a catalogue hold exactly the events a provider publishes: the provider is the
//! source of truth, so events it dropped go to the trash and come back if it republishes them.

use std::collections::BTreeSet;

use tracing::warn;
use yrs::TransactionMut;

use super::read::{catalogue, event, listed_events};
use super::{
    EventRecord, NewCatalogue, Status, add_catalogue_events, create_catalogue, create_event,
    delete_event, remove_event_attributes, rename_catalogue, restore_catalogue, restore_event,
    set_event_attributes, set_event_range,
};

/// A catalogue and the events it must hold; the uuids are the caller's.
#[derive(Debug, Clone, PartialEq)]
pub struct Wanted {
    pub catalogue: String,
    pub name: String,
    pub events: Vec<EventRecord>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub added: usize,
    pub changed: usize,
    pub removed: usize,
    pub restored: usize,
}

impl Counts {
    pub fn is_empty(&self) -> bool {
        *self == Counts::default()
    }
}

/// Makes the catalogue exist under `wanted.name` and list exactly `wanted.events`. Listed
/// events nobody wants go to the trash unless `keep` says to leave them. Holding the same
/// events twice writes nothing the second time. A wanted event the room cannot hold (a time
/// outside its range) is skipped with a warning.
pub fn hold(
    txn: &mut TransactionMut<'_>,
    wanted: &Wanted,
    keep: impl Fn(&EventRecord) -> bool,
) -> Counts {
    ensure_catalogue(txn, wanted);
    let listed = listed_events(txn, &wanted.catalogue);
    let mut counts = Counts::default();
    for record in &wanted.events {
        hold_event(txn, &wanted.catalogue, record, &mut counts);
    }
    counts.removed = trash_unwanted(txn, wanted, &listed, keep);
    counts
}

fn ensure_catalogue(txn: &mut TransactionMut<'_>, wanted: &Wanted) {
    match catalogue(txn, &wanted.catalogue) {
        None => {
            let new = NewCatalogue {
                uuid: wanted.catalogue.clone(),
                name: wanted.name.clone(),
                ..NewCatalogue::default()
            };
            if let Err(error) = create_catalogue(txn, &new) {
                warn!(catalogue = %wanted.catalogue, %error, "relay: catalogue not created");
            }
        }
        Some(existing) => {
            if existing.deleted {
                restore_catalogue(txn, &wanted.catalogue);
            }
            if existing.name != wanted.name {
                rename_catalogue(txn, &wanted.catalogue, &wanted.name);
            }
        }
    }
}

fn hold_event(
    txn: &mut TransactionMut<'_>,
    catalogue: &str,
    wanted: &EventRecord,
    counts: &mut Counts,
) {
    match event(txn, &wanted.uuid) {
        None => match create_event(txn, wanted) {
            Ok(_) => counts.added += 1,
            Err(error) => {
                warn!(event = %wanted.uuid, %error, "relay: event skipped");
                return;
            }
        },
        Some(current) => {
            if current.status == Status::Deleted {
                restore_event(txn, &wanted.uuid);
                counts.restored += 1;
            }
            if rewrite(txn, wanted, &current) {
                counts.changed += 1;
            }
        }
    }
    add_if_unlisted(txn, catalogue, &wanted.uuid);
}

fn differs(wanted: &EventRecord, current: &EventRecord) -> bool {
    (wanted.start, wanted.stop, &wanted.attributes)
        != (current.start, current.stop, &current.attributes)
}

fn rewrite(txn: &mut TransactionMut<'_>, wanted: &EventRecord, current: &EventRecord) -> bool {
    if !differs(wanted, current) {
        return false;
    }
    let stale: Vec<String> = current
        .attributes
        .keys()
        .filter(|key| !wanted.attributes.contains_key(*key))
        .cloned()
        .collect();
    let written = set_event_range(txn, &wanted.uuid, wanted.start, wanted.stop)
        .and_then(|_| set_event_attributes(txn, &wanted.uuid, &wanted.attributes));
    remove_event_attributes(txn, &wanted.uuid, &stale);
    if let Err(error) = written {
        warn!(event = %wanted.uuid, %error, "relay: event not rewritten");
    }
    true
}

fn add_if_unlisted(txn: &mut TransactionMut<'_>, catalogue: &str, uuid: &str) {
    if !listed_events(txn, catalogue)
        .iter()
        .any(|listed| listed == uuid)
    {
        add_catalogue_events(txn, catalogue, &[uuid.to_owned()]);
    }
}

fn trash_unwanted(
    txn: &mut TransactionMut<'_>,
    wanted: &Wanted,
    listed: &[String],
    keep: impl Fn(&EventRecord) -> bool,
) -> usize {
    let wanted_uuids: BTreeSet<&str> = wanted.events.iter().map(|e| e.uuid.as_str()).collect();
    let doomed: Vec<String> = listed
        .iter()
        .filter(|uuid| !wanted_uuids.contains(uuid.as_str()))
        .filter(|uuid| event(txn, uuid).is_some_and(|e| e.status != Status::Deleted && !keep(&e)))
        .cloned()
        .collect();
    doomed.iter().for_each(|uuid| {
        delete_event(txn, uuid);
    });
    doomed.len()
}
