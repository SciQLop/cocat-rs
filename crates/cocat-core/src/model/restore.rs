//! Making a room equal to a past state of itself, as one ordinary write: history is never
//! rewritten, and what was created since goes to the trash, so a restore can be undone.

use std::collections::BTreeSet;

use yrs::{Doc, In, Map as _, MapPrelim, MapRef, Out, ReadTxn, Transact, TransactionMut, WriteTxn};

use super::members::catalogue_events;
use super::read::{is_deleted, members, object};
use super::{CATALOGUES, EVENTS, Status};

/// Makes the events and catalogues equal to `past`; with a catalogue, only that catalogue
/// and the events that are its members now or were then. Events and catalogues absent from
/// `past` move to the trash. Origins, comments and the attribute registry stay as they are.
pub fn restore(txn: &mut TransactionMut<'_>, past: &Doc, catalogue: Option<&str>) {
    let past = past.transact();
    let (events, catalogues) = match catalogue {
        Some(uuid) => (
            &member_uuids(txn, uuid) | &member_uuids(&past, uuid),
            BTreeSet::from([uuid.to_owned()]),
        ),
        None => (
            &keys(txn, EVENTS) | &keys(&past, EVENTS),
            &keys(txn, CATALOGUES) | &keys(&past, CATALOGUES),
        ),
    };
    restore_root(txn, &past, EVENTS, &events);
    restore_root(txn, &past, CATALOGUES, &catalogues);
}

fn restore_root<P: ReadTxn>(
    txn: &mut TransactionMut<'_>,
    past: &P,
    root: &str,
    uuids: &BTreeSet<String>,
) {
    for uuid in uuids {
        match (object(past, root, uuid), object(txn, root, uuid)) {
            (Some(was), Some(is)) => restore_map(txn, past, &is, &was),
            (Some(was), None) => {
                txn.get_or_insert_map(root)
                    .insert(txn, uuid.as_str(), copy(past, &was));
            }
            (None, Some(is)) if !is_deleted(txn, &is) => {
                is.insert(txn, "status", Status::Deleted.name());
            }
            (None, _) => {}
        }
    }
}

/// Writes only the keys that differ, so restoring the present writes nothing.
fn restore_map<P: ReadTxn>(txn: &mut TransactionMut<'_>, past: &P, is: &MapRef, was: &MapRef) {
    let stale: Vec<String> = is
        .keys(txn)
        .filter(|key| !was.contains_key(past, key))
        .map(str::to_owned)
        .collect();
    for key in stale {
        is.remove(txn, &key);
    }
    let wanted: Vec<(String, Out)> = was
        .iter(past)
        .map(|(key, value)| (key.to_owned(), value))
        .collect();
    for (key, value) in wanted {
        match (value, is.get(txn, &key)) {
            (Out::Any(want), Some(Out::Any(have))) if want == have => {}
            (Out::YMap(want), Some(Out::YMap(have))) => restore_map(txn, past, &have, &want),
            (value, _) => {
                if let Some(value) = input(past, value) {
                    is.insert(txn, key, value);
                }
            }
        }
    }
}

fn copy<P: ReadTxn>(past: &P, map: &MapRef) -> MapPrelim {
    map.iter(past)
        .filter_map(|(key, value)| Some((key, input(past, value)?)))
        .collect()
}

/// The layout holds plain values and nested maps (a catalogue's event set) only.
fn input<P: ReadTxn>(past: &P, value: Out) -> Option<In> {
    match value {
        Out::Any(any) => Some(In::Any(any)),
        Out::YMap(map) => Some(In::Map(copy(past, &map))),
        _ => None,
    }
}

/// The events a catalogue lists, trashed ones included, and those its rule yields.
fn member_uuids<T: ReadTxn>(txn: &T, catalogue: &str) -> BTreeSet<String> {
    let mut uuids: BTreeSet<String> = object(txn, CATALOGUES, catalogue)
        .map(|map| members(txn, &map).into_iter().collect())
        .unwrap_or_default();
    if let Some(Ok(events)) = catalogue_events(txn, catalogue) {
        uuids.extend(events.into_iter().map(|event| event.uuid));
    }
    uuids
}

fn keys<T: ReadTxn>(txn: &T, root: &str) -> BTreeSet<String> {
    txn.get_map(root)
        .map(|map| map.keys(txn).map(str::to_owned).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use yrs::updates::decoder::Decode;
    use yrs::{ReadTxn, StateVector, Transact, Update};

    use super::super::{
        EventField, EventRecord, NewCatalogue, Status, add_catalogue_events, catalogue,
        create_catalogue, create_event, event, purge_event, rename_catalogue, set_event_field,
    };
    use super::*;
    use crate::time::Timestamp;

    fn record(uuid: &str) -> EventRecord {
        let at = |nanos| Timestamp::from_unix_nanos(nanos).unwrap();
        EventRecord::new(uuid, at(0), at(1_000))
    }

    fn new_catalogue(uuid: &str, events: &[&str]) -> impl FnOnce(&mut TransactionMut) {
        let uuid = uuid.to_owned();
        let events: Vec<String> = events.iter().map(|e| e.to_string()).collect();
        move |txn| {
            let new = NewCatalogue {
                uuid: uuid.clone(),
                name: uuid.clone(),
                ..NewCatalogue::default()
            };
            create_catalogue(txn, &new).unwrap();
            assert!(add_catalogue_events(txn, &uuid, &events));
        }
    }

    fn confidence(uuid: &str, value: f64) -> impl FnOnce(&mut TransactionMut) {
        let uuid = uuid.to_owned();
        move |txn| {
            let field = EventField::Confidence(Some(value));
            assert!(set_event_field(txn, &uuid, &field).unwrap());
        }
    }

    fn write(doc: &Doc, edit: impl FnOnce(&mut TransactionMut)) {
        edit(&mut doc.transact_mut());
    }

    /// The past, and a present that starts as a copy of it.
    fn past_and_present(edit: impl FnOnce(&mut TransactionMut)) -> (Doc, Doc) {
        let past = Doc::new();
        write(&past, edit);
        let present = Doc::new();
        let state = past
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        present
            .transact_mut()
            .apply_update(Update::decode_v1(&state).unwrap())
            .unwrap();
        (past, present)
    }

    /// Restores and gives the update the restore wrote.
    fn restored(present: &Doc, past: &Doc, catalogue: Option<&str>) -> Vec<u8> {
        let mut txn = present.transact_mut();
        restore(&mut txn, past, catalogue);
        txn.encode_update_v1()
    }

    fn listed(doc: &Doc, uuid: &str) -> Vec<String> {
        let txn = doc.transact();
        let mut events: Vec<String> = super::super::catalogue_events(&txn, uuid)
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|record| record.uuid)
            .collect();
        events.sort();
        events
    }

    #[test]
    fn restoring_brings_back_changed_fields() {
        let (past, present) = past_and_present(|txn| {
            create_event(txn, &record("a")).unwrap();
            confidence("a", 0.2)(txn);
        });
        write(&present, confidence("a", 0.9));
        write(&present, |txn| {
            assert!(set_event_field(txn, "a", &EventField::Author(Some("bob".into()))).unwrap());
        });

        restored(&present, &past, None);

        let txn = present.transact();
        assert_eq!(event(&txn, "a"), event(&past.transact(), "a"));
    }

    #[test]
    fn events_created_since_go_to_the_trash() {
        let (past, present) = past_and_present(|txn| {
            create_event(txn, &record("a")).unwrap();
        });
        write(&present, |txn| {
            create_event(txn, &record("new")).unwrap();
            new_catalogue("late", &["new"])(txn);
        });

        restored(&present, &past, None);

        let txn = present.transact();
        assert_eq!(event(&txn, "new").unwrap().status, Status::Deleted);
        assert!(catalogue(&txn, "late").unwrap().deleted);
        assert_eq!(event(&txn, "a").unwrap().status, Status::Confirmed);
    }

    #[test]
    fn a_purged_event_comes_back_whole() {
        let (past, present) = past_and_present(|txn| {
            create_event(txn, &record("a")).unwrap();
            confidence("a", 0.4)(txn);
            new_catalogue("c", &["a"])(txn);
        });
        write(&present, |txn| {
            assert!(purge_event(txn, "a"));
        });

        restored(&present, &past, None);

        let txn = present.transact();
        assert_eq!(event(&txn, "a"), event(&past.transact(), "a"));
        assert_eq!(listed(&present, "c"), ["a"]);
    }

    #[test]
    fn a_catalogue_restore_leaves_other_catalogues_alone() {
        let (past, present) = past_and_present(|txn| {
            create_event(txn, &record("a")).unwrap();
            create_event(txn, &record("b")).unwrap();
            new_catalogue("c1", &["a"])(txn);
            new_catalogue("c2", &[])(txn);
        });
        write(&present, |txn| {
            assert!(rename_catalogue(txn, "c1", "renamed"));
            assert!(rename_catalogue(txn, "c2", "renamed"));
            confidence("a", 0.9)(txn);
            confidence("b", 0.9)(txn);
            create_event(txn, &record("x")).unwrap();
            assert!(add_catalogue_events(txn, "c1", &["x".to_owned()]));
        });

        restored(&present, &past, Some("c1"));

        let txn = present.transact();
        assert_eq!(catalogue(&txn, "c1").unwrap().name, "c1");
        assert_eq!(listed(&present, "c1"), ["a"]);
        assert_eq!(event(&txn, "a").unwrap().confidence, None);
        assert_eq!(event(&txn, "x").unwrap().status, Status::Deleted);
        assert_eq!(catalogue(&txn, "c2").unwrap().name, "renamed");
        assert_eq!(event(&txn, "b").unwrap().confidence, Some(0.9));
    }

    #[test]
    fn restoring_the_present_changes_nothing() {
        let (past, present) = past_and_present(|txn| {
            create_event(txn, &record("a")).unwrap();
            confidence("a", 0.4)(txn);
            new_catalogue("c", &["a"])(txn);
        });

        assert_eq!(restored(&present, &past, None), [0, 0]);
        assert_eq!(restored(&present, &past, Some("c")), [0, 0]);
    }
}
