//! What a listener is told about: the document changes of one remote transaction, translated
//! from the yrs events that describe them.

use std::collections::{HashMap, HashSet};

use cocat_core::model;
use cocat_core::time::Timestamp;
use serde_json::{Map as JsonMap, Value};
use yrs::types::{EntryChange, Event, Events, Path, PathSegment};
use yrs::{Any, Map as _, MapRef, Out, ReadTxn, TransactionMut};

/// The fields of the document the change table names; anything else (tags, products, rating)
/// has no listener method and is ignored.
const NAME: &str = "name";
const ATTRIBUTES: &str = "attributes";
const MEMBERS: &str = "events";
const START: &str = "start";
const STOP: &str = "stop";

/// One change, named after the listener method that receives it.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    CatalogueAdded {
        uuid: String,
    },
    CatalogueRemoved {
        uuid: String,
    },
    CatalogueRenamed {
        uuid: String,
        name: String,
    },
    CatalogueAttributes {
        uuid: String,
        set: JsonMap<String, Value>,
        removed: Vec<String>,
    },
    EventsAdded {
        catalogue: String,
        uuids: Vec<String>,
    },
    EventsRemoved {
        catalogue: String,
        uuids: Vec<String>,
    },
    EventRange {
        uuid: String,
        start: Timestamp,
        stop: Timestamp,
    },
    EventAttributes {
        uuid: String,
        set: JsonMap<String, Value>,
        removed: Vec<String>,
    },
    EventRemoved {
        uuid: String,
    },
}

impl Change {
    /// The changes that share a slot are merged into one call: same kind, same target.
    fn slot(&self) -> (&'static str, &str) {
        match self {
            Change::CatalogueAdded { uuid } => ("catalogue_added", uuid),
            Change::CatalogueRemoved { uuid } => ("catalogue_removed", uuid),
            Change::CatalogueRenamed { uuid, .. } => ("catalogue_renamed", uuid),
            Change::CatalogueAttributes { uuid, .. } => ("catalogue_attributes", uuid),
            Change::EventsAdded { catalogue, .. } => ("events_added", catalogue),
            Change::EventsRemoved { catalogue, .. } => ("events_removed", catalogue),
            Change::EventRange { uuid, .. } => ("event_range", uuid),
            Change::EventAttributes { uuid, .. } => ("event_attributes", uuid),
            Change::EventRemoved { uuid } => ("event_removed", uuid),
        }
    }

    /// Folds `other` (already known to share this change's slot) into it.
    fn meld(&mut self, other: Change) {
        match (self, other) {
            (
                Change::CatalogueRenamed { name, .. },
                Change::CatalogueRenamed { name: later, .. },
            ) => *name = later,
            (
                Change::CatalogueAttributes { set, removed, .. },
                Change::CatalogueAttributes {
                    set: more,
                    removed: gone,
                    ..
                },
            )
            | (
                Change::EventAttributes { set, removed, .. },
                Change::EventAttributes {
                    set: more,
                    removed: gone,
                    ..
                },
            ) => {
                set.extend(more);
                removed.extend(gone);
            }
            (Change::EventsAdded { uuids, .. }, Change::EventsAdded { uuids: more, .. })
            | (Change::EventsRemoved { uuids, .. }, Change::EventsRemoved { uuids: more, .. }) => {
                uuids.extend(more);
            }
            (
                Change::EventRange { start, stop, .. },
                Change::EventRange {
                    start: later_start,
                    stop: later_stop,
                    ..
                },
            ) => {
                *start = later_start;
                *stop = later_stop;
            }
            // An addition or a removal is said once; a second identical one adds nothing.
            _ => {}
        }
    }
}

/// Which root map an observation came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Root {
    Catalogues,
    Events,
}

/// A map event held until its transaction commits, so one transaction becomes one batch.
pub(crate) struct Observation {
    root: Root,
    path: Vec<String>,
    keys: Vec<(String, EntryChange)>,
}

/// Keeps the map events of one root, for a transaction the caller knows is remote.
pub(crate) fn observe(
    root: Root,
    events: &Events<'_>,
    txn: &TransactionMut<'_>,
    into: &mut Vec<Observation>,
) {
    for event in events.iter() {
        let Event::Map(map) = event else {
            continue;
        };
        let Some(path) = plain_path(&event.path()) else {
            continue;
        };
        let keys = map
            .keys(txn)
            .iter()
            .map(|(key, change)| (key.to_string(), change.clone()))
            .collect();
        into.push(Observation { root, path, keys });
    }
}

/// The path as strings, or nothing when a segment is an array index: the document nests maps
/// only, and a path with an index names something no listener method can report.
fn plain_path(path: &Path) -> Option<Vec<String>> {
    path.iter()
        .map(|segment| match segment {
            PathSegment::Key(key) => Some(key.to_string()),
            PathSegment::Index(_) => None,
        })
        .collect()
}

/// Turns the observations of one transaction into the batch a listener receives, merging the
/// changes that share a slot.
pub(crate) fn finish(observations: &[Observation], txn: &TransactionMut<'_>) -> Vec<Change> {
    let created = created(observations);
    let mut changes = Vec::new();
    for observation in observations {
        let under_a_new_object = observation
            .path
            .first()
            .is_some_and(|uuid| created.contains(uuid));
        let membership = observation.path.len() == 2 && observation.path[1] == MEMBERS;
        if under_a_new_object && !membership {
            // The object was created by this very transaction: its fields are part of the
            // creation, not a rename or an attribute change on top of it. Which events the
            // new catalogue lists is still news, because `catalogue_added` says nothing
            // about its contents.
            continue;
        }
        translate(observation, txn, &mut changes);
    }
    merge(changes)
}

/// The uuids whose map was inserted at a root in this transaction.
fn created(observations: &[Observation]) -> HashSet<String> {
    let mut uuids = HashSet::new();
    for observation in observations.iter().filter(|one| one.path.is_empty()) {
        for (uuid, change) in &observation.keys {
            if matches!(change, EntryChange::Inserted(_)) {
                uuids.insert(uuid.clone());
            }
        }
    }
    uuids
}

fn translate(observation: &Observation, txn: &TransactionMut<'_>, into: &mut Vec<Change>) {
    if observation.path.is_empty() {
        roots(observation, txn, into);
        return;
    }
    let uuid = observation.path[0].as_str();
    let field = observation.path.get(1).map(String::as_str);
    match (observation.root, observation.path.len(), field) {
        (Root::Catalogues, 1, _) => renames(uuid, &observation.keys, into),
        (Root::Catalogues, 2, Some(MEMBERS)) => memberships(uuid, &observation.keys, into),
        (Root::Catalogues, 2, Some(ATTRIBUTES)) => {
            attributes(true, uuid, &observation.keys, into);
        }
        (Root::Events, 1, _) => range(txn, uuid, &observation.keys, into),
        (Root::Events, 2, Some(ATTRIBUTES)) => attributes(false, uuid, &observation.keys, into),
        _ => {}
    }
}

fn roots(observation: &Observation, txn: &TransactionMut<'_>, into: &mut Vec<Change>) {
    for (uuid, change) in &observation.keys {
        match (observation.root, change) {
            (Root::Catalogues, EntryChange::Inserted(_)) => {
                into.push(Change::CatalogueAdded { uuid: uuid.clone() });
                let uuids = members(txn, uuid);
                if !uuids.is_empty() {
                    into.push(Change::EventsAdded {
                        catalogue: uuid.clone(),
                        uuids,
                    });
                }
            }
            (Root::Catalogues, EntryChange::Removed(_)) => {
                into.push(Change::CatalogueRemoved { uuid: uuid.clone() })
            }
            (Root::Events, EntryChange::Removed(_)) => {
                into.push(Change::EventRemoved { uuid: uuid.clone() })
            }
            // A new event is announced through `events_added` for the catalogue listing it;
            // there is no listener method for the event map itself.
            _ => {}
        }
    }
}

/// The events a catalogue lists now. A deep observer does not report the keys of a sub-map
/// created in the same transaction, so a catalogue that arrives with its events is read from
/// the document instead.
fn members(txn: &TransactionMut<'_>, catalogue: &str) -> Vec<String> {
    let Some(catalogues) = txn.get_map(model::CATALOGUES) else {
        return Vec::new();
    };
    let Some(Out::YMap(object)) = catalogues.get(txn, catalogue) else {
        return Vec::new();
    };
    let Some(Out::YMap(members)) = object.get(txn, MEMBERS) else {
        return Vec::new();
    };
    members
        .iter(txn)
        .map(|(uuid, _)| uuid.to_string())
        .collect()
}

fn renames(uuid: &str, keys: &[(String, EntryChange)], into: &mut Vec<Change>) {
    for (key, change) in keys {
        if key != NAME {
            continue;
        }
        let name = match change {
            EntryChange::Inserted(Out::Any(Any::String(name)))
            | EntryChange::Updated(_, Out::Any(Any::String(name))) => name,
            _ => continue,
        };
        into.push(Change::CatalogueRenamed {
            uuid: uuid.to_owned(),
            name: name.to_string(),
        });
    }
}

fn memberships(catalogue: &str, keys: &[(String, EntryChange)], into: &mut Vec<Change>) {
    for (uuid, change) in keys {
        match change {
            EntryChange::Inserted(_) => into.push(Change::EventsAdded {
                catalogue: catalogue.to_owned(),
                uuids: vec![uuid.clone()],
            }),
            EntryChange::Removed(_) => into.push(Change::EventsRemoved {
                catalogue: catalogue.to_owned(),
                uuids: vec![uuid.clone()],
            }),
            EntryChange::Updated(_, _) => {}
        }
    }
}

fn attributes(catalogue: bool, uuid: &str, keys: &[(String, EntryChange)], into: &mut Vec<Change>) {
    let mut set = JsonMap::new();
    let mut removed = Vec::new();
    for (key, change) in keys {
        match change {
            EntryChange::Inserted(out) | EntryChange::Updated(_, out) => {
                if let Some(value) = json(out) {
                    set.insert(key.clone(), value);
                }
            }
            EntryChange::Removed(_) => removed.push(key.clone()),
        }
    }
    if set.is_empty() && removed.is_empty() {
        return;
    }
    let uuid = uuid.to_owned();
    into.push(if catalogue {
        Change::CatalogueAttributes { uuid, set, removed }
    } else {
        Change::EventAttributes { uuid, set, removed }
    });
}

fn range(
    txn: &TransactionMut<'_>,
    uuid: &str,
    keys: &[(String, EntryChange)],
    into: &mut Vec<Change>,
) {
    if !keys.iter().any(|(key, _)| key == START || key == STOP) {
        return;
    }
    // Both ends of the range are sent together, so the listener redraws once with a
    // consistent pair; the document always holds the current pair.
    let Some((start, stop)) = times(txn, uuid) else {
        return;
    };
    into.push(Change::EventRange {
        uuid: uuid.to_owned(),
        start,
        stop,
    });
}

fn times(txn: &TransactionMut<'_>, uuid: &str) -> Option<(Timestamp, Timestamp)> {
    let events = txn.get_map(model::EVENTS)?;
    let Out::YMap(event) = events.get(txn, uuid)? else {
        return None;
    };
    let start = time(txn, &event, START)?;
    let stop = time(txn, &event, STOP)?;
    Some((start, stop))
}

fn time(txn: &TransactionMut<'_>, event: &MapRef, key: &str) -> Option<Timestamp> {
    match event.get(txn, key) {
        Some(Out::Any(Any::String(text))) => Timestamp::parse(&text),
        _ => None,
    }
}

fn json(out: &Out) -> Option<Value> {
    match out {
        Out::Any(any) => serde_json::to_value(any).ok(),
        _ => None,
    }
}

/// Folds the changes that share a slot, keeping the batch in the order they were reported.
fn merge(changes: Vec<Change>) -> Vec<Change> {
    let mut merged: Vec<Change> = Vec::with_capacity(changes.len());
    let mut slots: HashMap<(&'static str, String), usize> = HashMap::new();
    for change in changes {
        let (kind, target) = change.slot();
        match slots.get(&(kind, target.to_owned())) {
            Some(index) => merged[*index].meld(change),
            None => {
                slots.insert((kind, target.to_owned()), merged.len());
                merged.push(change);
            }
        }
    }
    for change in &mut merged {
        match change {
            Change::EventsAdded { uuids, .. } | Change::EventsRemoved { uuids, .. } => {
                sorted(uuids)
            }
            Change::CatalogueAttributes { removed, .. }
            | Change::EventAttributes { removed, .. } => sorted(removed),
            _ => {}
        }
    }
    merged
}

/// The document holds these as maps used as sets, so a listener gets them sorted: one call's
/// contents must not depend on the order a hash map happened to report.
fn sorted(items: &mut Vec<String>) {
    items.sort_unstable();
    items.dedup();
}
