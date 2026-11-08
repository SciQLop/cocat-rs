//! A catalogue on its own, as a submission to the public catalogues carries it, and taking
//! one into a public catalogue. Both are restores of one catalogue: the first into an empty
//! document, the second into the public one, so nothing about events and catalogues is
//! copied twice.

use std::collections::BTreeSet;

use yrs::{Doc, ReadTxn, Transact, TransactionMut};

use super::members::catalogue_events;
use super::registry::{attribute_types, register_attribute, set_attribute_info};
use super::restore::restore;
use super::write::mark_layout;
use super::{AttributeInfo, EventRecord};

/// A new document holding the catalogue and its members' events, trashed ones included,
/// with the types of the attributes those events carry and the layout marker. Nothing else
/// of `source` comes along.
pub fn extract(source: &Doc, catalogue: &str) -> Doc {
    let doc = Doc::new();
    {
        let mut txn = doc.transact_mut();
        mark_layout(&mut txn);
        restore(&mut txn, source, Some(catalogue));
        let keys = attribute_keys(&txn, catalogue);
        let types = attribute_types(&source.transact());
        register(
            &mut txn,
            types.into_iter().filter(|(key, _)| keys.contains(key)),
        );
    }
    doc
}

/// Makes the catalogue and its events equal to `proposed`'s: events it does not hold go to
/// the trash. Attribute types the room lacks are taken from `proposed`; those it has stay.
pub fn adopt(txn: &mut TransactionMut<'_>, proposed: &Doc, catalogue: &str) {
    restore(txn, proposed, Some(catalogue));
    let known = attribute_types(txn);
    let types = attribute_types(&proposed.transact());
    register(
        txn,
        types
            .into_iter()
            .filter(|(key, _)| !known.contains_key(key)),
    );
}

fn register(txn: &mut TransactionMut<'_>, types: impl Iterator<Item = (String, AttributeInfo)>) {
    for (key, info) in types {
        register_attribute(txn, &key, info.kind);
        set_attribute_info(txn, &key, info.unit.as_deref(), info.description.as_deref());
    }
}

fn attribute_keys<T: ReadTxn>(txn: &T, catalogue: &str) -> BTreeSet<String> {
    let events: Vec<EventRecord> = match catalogue_events(txn, catalogue) {
        Some(Ok(events)) => events,
        _ => Vec::new(),
    };
    events
        .into_iter()
        .flat_map(|event| event.attributes.into_iter().map(|(key, _)| key))
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};
    use yrs::{Doc, Transact};

    use super::super::{
        AttributeType, EventField, EventRecord, NewCatalogue, Status, add_catalogue_events,
        attribute_types, catalogue, catalogue_events, catalogues, create_catalogue, create_event,
        event, set_event_attributes, set_event_field,
    };
    use super::*;
    use crate::time::Timestamp;

    fn record(uuid: &str) -> EventRecord {
        let at = |nanos| Timestamp::from_unix_nanos(nanos).unwrap();
        EventRecord::new(uuid, at(0), at(1_000))
    }

    fn attributes(value: serde_json::Value) -> Map<String, serde_json::Value> {
        match value {
            serde_json::Value::Object(map) => map,
            _ => Map::new(),
        }
    }

    /// Catalogue `c` lists `a` (with a `speed` number) and `b`; `x` is elsewhere, with a
    /// `colour` text in catalogue `other`.
    fn project() -> Doc {
        let doc = Doc::new();
        let mut txn = doc.transact_mut();
        for uuid in ["a", "b", "x"] {
            create_event(&mut txn, &record(uuid)).unwrap();
        }
        set_event_attributes(&mut txn, "a", &attributes(json!({"speed": 400}))).unwrap();
        set_event_attributes(&mut txn, "x", &attributes(json!({"colour": "red"}))).unwrap();
        for (uuid, events) in [("c", vec!["a", "b"]), ("other", vec!["x"])] {
            let new = NewCatalogue {
                uuid: uuid.to_owned(),
                name: uuid.to_owned(),
                ..NewCatalogue::default()
            };
            create_catalogue(&mut txn, &new).unwrap();
            let events: Vec<String> = events.into_iter().map(str::to_owned).collect();
            assert!(add_catalogue_events(&mut txn, uuid, &events));
        }
        drop(txn);
        doc
    }

    fn listed(doc: &Doc, uuid: &str) -> Vec<String> {
        let txn = doc.transact();
        let mut uuids: Vec<String> = catalogue_events(&txn, uuid)
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|event| event.uuid)
            .collect();
        uuids.sort();
        uuids
    }

    #[test]
    fn an_extract_holds_the_catalogue_its_events_and_their_types_only() {
        let source = project();

        let alone = extract(&source, "c");

        let txn = alone.transact();
        let names: Vec<String> = catalogues(&txn).into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["c"]);
        assert_eq!(listed(&alone, "c"), ["a", "b"]);
        assert_eq!(event(&txn, "a"), event(&source.transact(), "a"));
        assert_eq!(event(&txn, "x"), None);
        let types: Vec<String> = attribute_types(&txn).into_keys().collect();
        assert_eq!(types, ["speed"]);
        assert!(!super::super::is_v1(&txn), "it says which layout it holds");
    }

    #[test]
    fn adopting_takes_the_proposal_and_trashes_what_it_left_out() {
        let public = project();
        let copy = extract(&public, "c");
        {
            let mut txn = copy.transact_mut();
            set_event_field(&mut txn, "a", &EventField::Confidence(Some(0.5))).unwrap();
            create_event(&mut txn, &record("new")).unwrap();
            set_event_attributes(&mut txn, "new", &attributes(json!({"flag": true}))).unwrap();
            assert!(add_catalogue_events(&mut txn, "c", &["new".to_owned()]));
            assert!(super::super::remove_catalogue_events(
                &mut txn,
                "c",
                &["b".to_owned()]
            ));
        }
        let proposed = extract(&copy, "c");

        adopt(&mut public.transact_mut(), &proposed, "c");

        let txn = public.transact();
        assert_eq!(event(&txn, "a").unwrap().confidence, Some(0.5));
        assert_eq!(event(&txn, "new"), event(&proposed.transact(), "new"));
        assert_eq!(event(&txn, "b").unwrap().status, Status::Deleted);
        assert_eq!(listed(&public, "c"), ["a", "new"]);
        assert_eq!(event(&txn, "x").unwrap().status, Status::Confirmed);
        assert_eq!(catalogue(&txn, "other").unwrap().event_count, 1);
        let types = attribute_types(&txn);
        assert_eq!(types["flag"].kind, AttributeType::Bool);
        assert_eq!(types["colour"].kind, AttributeType::Text);
    }

    #[test]
    fn adopting_the_same_catalogue_writes_nothing() {
        let public = project();
        let proposed = extract(&public, "c");

        let mut txn = public.transact_mut();
        adopt(&mut txn, &proposed, "c");

        assert_eq!(txn.encode_update_v1(), [0, 0]);
    }
}
