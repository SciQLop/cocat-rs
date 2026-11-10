//! Making a catalogue hold exactly the events a provider publishes: the provider is the
//! source of truth, so events it dropped go to the trash and come back if it republishes them.

use std::collections::BTreeMap;

use tracing::warn;
use yrs::TransactionMut;

use super::read::{catalogue, event, listed_events};
use super::{
    EventRecord, NewCatalogue, Status, WriteError, add_catalogue_events, create_catalogue,
    create_event, delete_event, remove_event_attributes, rename_catalogue, restore_catalogue,
    restore_event, set_event_attributes, set_event_range,
};

/// A catalogue and the events it must hold; the uuids are the caller's. A provider sets only
/// the start, stop and attributes of its events: `hold` keeps no other field up to date.
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
    /// Wanted events the room cannot hold (a time outside its range): left as they were.
    pub skipped: usize,
}

impl Counts {
    pub fn is_empty(&self) -> bool {
        *self == Counts::default()
    }
}

impl std::ops::AddAssign for Counts {
    fn add_assign(&mut self, other: Self) {
        self.added += other.added;
        self.changed += other.changed;
        self.removed += other.removed;
        self.restored += other.restored;
        self.skipped += other.skipped;
    }
}

/// Makes the catalogue exist under `wanted.name` and list exactly `wanted.events` (the last of
/// two events with one uuid wins). Listed events nobody wants go to the trash unless `keep`
/// says to leave them. Holding the same events twice writes nothing the second time.
pub fn hold(
    txn: &mut TransactionMut<'_>,
    wanted: &Wanted,
    keep: impl Fn(&EventRecord) -> bool,
) -> Counts {
    let mut counts = Counts::default();
    if !ensure_catalogue(txn, wanted) {
        return counts;
    }
    let listed = listed_events(txn, &wanted.catalogue);
    let events: BTreeMap<&str, &EventRecord> = wanted
        .events
        .iter()
        .map(|record| (record.uuid.as_str(), record))
        .collect();
    let to_list: Vec<String> = events
        .values()
        .filter(|record| hold_event(txn, record, &mut counts))
        .map(|record| record.uuid.clone())
        .filter(|uuid| listed.binary_search(uuid).is_err())
        .collect();
    add_catalogue_events(txn, &wanted.catalogue, &to_list);
    counts.removed = trash_unwanted(txn, &listed, &events, keep);
    counts
}

fn ensure_catalogue(txn: &mut TransactionMut<'_>, wanted: &Wanted) -> bool {
    match catalogue(txn, &wanted.catalogue) {
        None => {
            let new = NewCatalogue {
                uuid: wanted.catalogue.clone(),
                name: wanted.name.clone(),
                ..NewCatalogue::default()
            };
            create_catalogue(txn, &new)
                .inspect_err(|error| {
                    warn!(catalogue = %wanted.catalogue, %error, "relay: catalogue not created");
                })
                .is_ok()
        }
        Some(existing) => {
            if existing.deleted {
                restore_catalogue(txn, &wanted.catalogue);
            }
            if existing.name != wanted.name {
                rename_catalogue(txn, &wanted.catalogue, &wanted.name);
            }
            true
        }
    }
}

/// Brings one event to what is wanted; false when the room cannot hold it.
fn hold_event(txn: &mut TransactionMut<'_>, wanted: &EventRecord, counts: &mut Counts) -> bool {
    let outcome = match event(txn, &wanted.uuid) {
        None => create_event(txn, wanted).map(|_| counts.added += 1),
        Some(current) => update_event(txn, wanted, &current, counts),
    };
    outcome
        .inspect_err(|error| {
            warn!(event = %wanted.uuid, %error, "relay: event skipped");
            counts.skipped += 1;
        })
        .is_ok()
}

fn update_event(
    txn: &mut TransactionMut<'_>,
    wanted: &EventRecord,
    current: &EventRecord,
    counts: &mut Counts,
) -> Result<(), WriteError> {
    if current.status == Status::Deleted {
        restore_event(txn, &wanted.uuid);
        counts.restored += 1;
    }
    if differs(wanted, current) {
        rewrite(txn, wanted, current)?;
        counts.changed += 1;
    }
    Ok(())
}

fn differs(wanted: &EventRecord, current: &EventRecord) -> bool {
    (wanted.start, wanted.stop, &wanted.attributes)
        != (current.start, current.stop, &current.attributes)
}

fn rewrite(
    txn: &mut TransactionMut<'_>,
    wanted: &EventRecord,
    current: &EventRecord,
) -> Result<(), WriteError> {
    let stale: Vec<String> = current
        .attributes
        .keys()
        .filter(|key| !wanted.attributes.contains_key(*key))
        .cloned()
        .collect();
    set_event_range(txn, &wanted.uuid, wanted.start, wanted.stop)?;
    set_event_attributes(txn, &wanted.uuid, &wanted.attributes)?;
    remove_event_attributes(txn, &wanted.uuid, &stale);
    Ok(())
}

fn trash_unwanted(
    txn: &mut TransactionMut<'_>,
    listed: &[String],
    wanted: &BTreeMap<&str, &EventRecord>,
    keep: impl Fn(&EventRecord) -> bool,
) -> usize {
    let doomed: Vec<&String> = listed
        .iter()
        .filter(|uuid| !wanted.contains_key(uuid.as_str()))
        .filter(|uuid| event(txn, uuid).is_some_and(|e| e.status != Status::Deleted && !keep(&e)))
        .collect();
    doomed.iter().for_each(|uuid| {
        delete_event(txn, uuid);
    });
    doomed.len()
}
