//! What a listener is told about: the document changes of one remote transaction, translated
//! from the yrs events that describe them.

use std::collections::{HashMap, HashSet};

use cocat_core::model::{self, ATTRIBUTE_PREFIX, Status};
use cocat_core::time::Timestamp;
use serde_json::{Map as JsonMap, Value};
use yrs::types::{EntryChange, Event, Events, Path, PathSegment};
use yrs::{Any, Map as _, Out, ReadTxn, TransactionMut};

/// The fields of the document the change table names. A catalogue's tags, columns, author
/// and creation time have no listener method and are ignored.
const NAME: &str = "name";
const RULE: &str = "rule";
const STATUS: &str = "status";
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
    /// The fixed fields other than the range, with their values as the document stores
    /// them (times in nanoseconds); a field back at its default is `removed`.
    EventFields {
        uuid: String,
        set: JsonMap<String, Value>,
        removed: Vec<String>,
    },
    CatalogueRule {
        uuid: String,
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
            Change::EventFields { uuid, .. } => ("event_fields", uuid),
            Change::CatalogueRule { uuid } => ("catalogue_rule", uuid),
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
            )
            | (
                Change::EventFields { set, removed, .. },
                Change::EventFields {
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
    let keys = &observation.keys;
    match (observation.root, observation.path.len(), field) {
        (Root::Catalogues, 1, _) => {
            renames(uuid, keys, into);
            trash(uuid, keys, into);
            rule(uuid, keys, into);
            attributes(true, uuid, keys, into);
        }
        (Root::Catalogues, 2, Some(MEMBERS)) => memberships(uuid, keys, into),
        (Root::Events, 1, _) => {
            range(txn, uuid, keys, into);
            attributes(false, uuid, keys, into);
            fields(uuid, keys, into);
        }
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

/// A catalogue moved to the trash is gone for a listener, and one restored is new again.
fn trash(uuid: &str, keys: &[(String, EntryChange)], into: &mut Vec<Change>) {
    for (key, change) in keys {
        if key != STATUS {
            continue;
        }
        let uuid = uuid.to_owned();
        into.push(match change {
            EntryChange::Removed(_) => Change::CatalogueAdded { uuid },
            EntryChange::Inserted(out) | EntryChange::Updated(_, out) if is_deleted(out) => {
                Change::CatalogueRemoved { uuid }
            }
            _ => continue,
        });
    }
}

fn is_deleted(out: &Out) -> bool {
    matches!(out, Out::Any(Any::String(name)) if Status::from_name(name) == Some(Status::Deleted))
}

fn rule(uuid: &str, keys: &[(String, EntryChange)], into: &mut Vec<Change>) {
    if keys.iter().any(|(key, _)| key == RULE) {
        into.push(Change::CatalogueRule {
            uuid: uuid.to_owned(),
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
    let attributes = keys.iter().filter_map(|(key, change)| {
        key.strip_prefix(ATTRIBUTE_PREFIX)
            .map(|name| (name.to_owned(), change))
    });
    let Some((set, removed)) = split(attributes) else {
        return;
    };
    let uuid = uuid.to_owned();
    into.push(if catalogue {
        Change::CatalogueAttributes { uuid, set, removed }
    } else {
        Change::EventAttributes { uuid, set, removed }
    });
}

fn fields(uuid: &str, keys: &[(String, EntryChange)], into: &mut Vec<Change>) {
    let fields = keys
        .iter()
        .filter(|(key, _)| key != START && key != STOP && !key.starts_with(ATTRIBUTE_PREFIX))
        .map(|(key, change)| (key.clone(), change));
    if let Some((set, removed)) = split(fields) {
        into.push(Change::EventFields {
            uuid: uuid.to_owned(),
            set,
            removed,
        });
    }
}

/// The keys written, with their values, and the keys removed; `None` when there are none.
fn split<'a>(
    keys: impl Iterator<Item = (String, &'a EntryChange)>,
) -> Option<(JsonMap<String, Value>, Vec<String>)> {
    let mut set = JsonMap::new();
    let mut removed = Vec::new();
    for (key, change) in keys {
        match change {
            EntryChange::Inserted(out) | EntryChange::Updated(_, out) => {
                if let Some(value) = json(out) {
                    set.insert(key, value);
                }
            }
            EntryChange::Removed(_) => removed.push(key),
        }
    }
    (!set.is_empty() || !removed.is_empty()).then_some((set, removed))
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
    let Some(event) = model::event(txn, uuid) else {
        return;
    };
    into.push(Change::EventRange {
        uuid: uuid.to_owned(),
        start: event.start,
        stop: event.stop,
    });
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
            | Change::EventAttributes { removed, .. }
            | Change::EventFields { removed, .. } => sorted(removed),
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

#[cfg(test)]
mod tests {
    use cocat_core::model::{self, EventField, EventRecord, NewCatalogue};
    use cocat_core::time::Timestamp;
    use serde_json::{Map, json};
    use tempfile::TempDir;
    use yrs::updates::decoder::Decode;
    use yrs::{Doc, ReadTxn, Transact, TransactionMut, Update};

    use super::Change;
    use crate::room::{REMOTE_ORIGIN, Room};

    const CATALOGUE: &str = "6f9c1a52-0b0d-4f4e-9d5e-1b2c3d4e5f60";
    const EVENT: &str = "aaaaaaaa-1111-4111-8111-111111111111";

    /// A room and a second replica whose edits reach the room as the server would send them.
    struct Pair {
        _dir: TempDir,
        room: Room,
        source: Doc,
    }

    impl Pair {
        fn new() -> Pair {
            let dir = TempDir::new().unwrap();
            let room = Room::open(dir.path(), "room").unwrap();
            let pair = Pair {
                _dir: dir,
                room,
                source: Doc::new(),
            };
            pair.relay(|txn| {
                let catalogue = NewCatalogue {
                    uuid: CATALOGUE.to_owned(),
                    name: "alpha".to_owned(),
                    ..NewCatalogue::default()
                };
                model::create_catalogue(txn, &catalogue).unwrap();
                let at = Timestamp::parse("2025-01-31 10:00:00").unwrap();
                model::create_event(txn, &EventRecord::new(EVENT, at, at)).unwrap();
            });
            pair
        }

        /// The batch the room reports for one remote transaction made of `edit`.
        fn relay(&self, edit: impl FnOnce(&mut TransactionMut<'_>)) -> Vec<Change> {
            let mut changes = self.room.changes();
            let before = self.source.transact().state_vector();
            edit(&mut self.source.transact_mut());
            let update = self.source.transact().encode_state_as_update_v1(&before);
            self.room
                .doc()
                .transact_mut_with(REMOTE_ORIGIN)
                .apply_update(Update::decode_v1(&update).unwrap())
                .unwrap();
            changes.try_recv().unwrap_or_default()
        }
    }

    fn map(value: serde_json::Value) -> Map<String, serde_json::Value> {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn a_fixed_field_is_an_event_fields_change() {
        let pair = Pair::new();
        let batch = pair.relay(|txn| {
            model::set_event_field(txn, EVENT, &EventField::Confidence(Some(0.5))).unwrap();
            model::delete_event(txn, EVENT);
        });
        assert_eq!(
            batch,
            [Change::EventFields {
                uuid: EVENT.to_owned(),
                set: map(json!({"confidence": 0.5, "status": "deleted"})),
                removed: vec![],
            }]
        );

        let batch = pair.relay(|txn| {
            model::restore_event(txn, EVENT);
        });
        assert_eq!(
            batch,
            [Change::EventFields {
                uuid: EVENT.to_owned(),
                set: Map::new(),
                removed: vec!["status".to_owned()],
            }]
        );
    }

    #[test]
    fn a_flat_attribute_is_an_attributes_change_without_its_prefix() {
        let pair = Pair::new();
        let batch = pair.relay(|txn| {
            model::set_event_attributes(txn, EVENT, &map(json!({"q": 2}))).unwrap();
            model::set_catalogue_attributes(txn, CATALOGUE, &map(json!({"m": "MMS"}))).unwrap();
        });
        assert_eq!(batch.len(), 2, "{batch:?}");
        assert!(batch.contains(&Change::EventAttributes {
            uuid: EVENT.to_owned(),
            set: map(json!({"q": 2})),
            removed: vec![],
        }));
        assert!(batch.contains(&Change::CatalogueAttributes {
            uuid: CATALOGUE.to_owned(),
            set: map(json!({"m": "MMS"})),
            removed: vec![],
        }));
    }

    #[test]
    fn a_range_change_carries_both_times() {
        let pair = Pair::new();
        let start = Timestamp::parse("2025-02-01 00:00:00.5").unwrap();
        let stop = Timestamp::parse("2025-02-01 00:01:00").unwrap();
        let batch = pair.relay(|txn| {
            model::set_event_range(txn, EVENT, start, stop).unwrap();
        });
        assert_eq!(
            batch,
            [Change::EventRange {
                uuid: EVENT.to_owned(),
                start,
                stop,
            }]
        );
    }

    #[test]
    fn a_rule_change_names_its_catalogue() {
        let pair = Pair::new();
        let rule = json!({"all": true});
        let batch = pair.relay(|txn| {
            model::set_catalogue_rule(txn, CATALOGUE, Some(&rule)).unwrap();
        });
        assert_eq!(
            batch,
            [Change::CatalogueRule {
                uuid: CATALOGUE.to_owned()
            }]
        );
    }

    #[test]
    fn a_catalogue_in_the_trash_is_removed_and_restored_is_added() {
        let pair = Pair::new();
        let uuid = CATALOGUE.to_owned();
        let batch = pair.relay(|txn| {
            model::delete_catalogue(txn, CATALOGUE);
        });
        assert_eq!(batch, [Change::CatalogueRemoved { uuid: uuid.clone() }]);

        let batch = pair.relay(|txn| {
            model::restore_catalogue(txn, CATALOGUE);
        });
        assert_eq!(batch, [Change::CatalogueAdded { uuid }]);
        assert_eq!(
            model::catalogue(&pair.room.doc().transact(), CATALOGUE).map(|c| c.deleted),
            Some(false)
        );
    }
}
