//! Moving a room Python cocat wrote (layout v1) to layout v2.

use std::fs;

use cocat_core::model::{self, AttributeType};
use cocat_core::store::FileRoomStore;
use cocat_core::time::Timestamp;
use serde::Deserialize;
use serde_json::{Map, Value};
use tempfile::TempDir;
use yrs::updates::decoder::Decode;
use yrs::{Doc, ReadTxn as _, Transact, Update};

/// Written by Python cocat's own client; see `fixtures/make_catalogues.py`.
const FIXTURE: &[u8] = include_bytes!("fixtures/catalogues.y");
const EXPECTED: &str = include_str!("fixtures/catalogues.expected.json");
const ALPHA: &str = "6f9c1a52-0b0d-4f4e-9d5e-1b2c3d4e5f60";
const BETA: &str = "0e4d2c31-5a6b-4c7d-8e9f-0a1b2c3d4e5f";
const MALFORMED: &str = "11111111-2222-4333-8444-555555555555";
const DANGLING: &str = "99999999-8888-4777-8666-555555555555";

#[derive(Deserialize)]
struct Expected {
    catalogues: Vec<ExpectedCatalogue>,
    events: Vec<ExpectedEvent>,
}

#[derive(Deserialize)]
struct ExpectedCatalogue {
    uuid: String,
    name: String,
    author: String,
    tags: Vec<String>,
    attributes: Map<String, Value>,
}

#[derive(Deserialize)]
struct ExpectedEvent {
    uuid: String,
    start: String,
    stop: String,
    author: String,
    tags: Vec<String>,
    products: Vec<String>,
    rating: Option<f64>,
    attributes: Map<String, Value>,
}

/// pycrdt hands Python every number as a float, so the JSON Python printed has `2.0` where
/// the document holds `2`. Numbers compare by value, everything else exactly.
fn same(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => left.as_f64() == right.as_f64(),
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .all(|(key, one)| right.get(key).is_some_and(|other| same(one, other)))
        }
        _ => left == right,
    }
}

fn v1_document() -> Doc {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("room.y"), FIXTURE).unwrap();
    let (_, updates) = FileRoomStore::open(dir.path(), "room").unwrap();
    let doc = Doc::new();
    for update in &updates {
        let update = Update::decode_v1(update).unwrap();
        doc.transact_mut().apply_update(update).unwrap();
    }
    doc
}

/// The fixture migrated, as a second replica that only receives the migration's update on
/// top of the v1 history: what a client sees after the server migrated the room.
fn migrated() -> Doc {
    let doc = v1_document();
    assert!(model::is_v1(&doc.transact()));
    let update = model::migrate_v1(&doc)
        .unwrap()
        .expect("a v1 room migrates");

    let replica = v1_document();
    replica
        .transact_mut()
        .apply_update(Update::decode_v1(&update).unwrap())
        .unwrap();
    replica
}

#[test]
fn every_catalogue_field_is_carried_over() {
    let doc = migrated();
    let txn = doc.transact();
    let expected: Expected = serde_json::from_str(EXPECTED).unwrap();

    let catalogues = model::catalogues(&txn);
    assert_eq!(catalogues.len(), expected.catalogues.len());
    for (catalogue, want) in catalogues.iter().zip(&expected.catalogues) {
        assert_eq!(catalogue.uuid, want.uuid);
        assert_eq!(catalogue.name, want.name);
        assert_eq!(catalogue.author.as_deref(), Some(want.author.as_str()));
        assert_eq!(catalogue.tags, want.tags);
        assert!(
            same(
                &Value::Object(catalogue.attributes.clone()),
                &Value::Object(want.attributes.clone())
            ),
            "{} attributes read as {:?}",
            catalogue.name,
            catalogue.attributes
        );
    }
}

#[test]
fn every_event_field_is_carried_over_and_the_rating_becomes_a_confidence() {
    let doc = migrated();
    let txn = doc.transact();
    let expected: Expected = serde_json::from_str(EXPECTED).unwrap();

    let events = model::catalogue_events(&txn, ALPHA).unwrap().unwrap();
    assert_eq!(events.len(), expected.events.len());
    for (event, want) in events.iter().zip(&expected.events) {
        assert_eq!(event.uuid, want.uuid);
        assert_eq!(event.start, Timestamp::parse(&want.start).unwrap());
        assert_eq!(event.stop, Timestamp::parse(&want.stop).unwrap());
        assert_eq!(event.author.as_deref(), Some(want.author.as_str()));
        assert_eq!(event.tags, want.tags);
        assert_eq!(event.products, want.products);
        assert_eq!(event.confidence, want.rating.map(|rating| rating / 10.0));
        assert_eq!(event.attributes, want.attributes);
    }
    let confidences: Vec<Option<f64>> = events.iter().map(|event| event.confidence).collect();
    assert_eq!(confidences, [Some(0.3), None, Some(0.1)]);
}

#[test]
fn the_migrated_room_is_v2_and_registers_its_attributes() {
    let doc = migrated();
    let txn = doc.transact();

    assert!(!model::is_v1(&txn));
    let types = model::attribute_types(&txn);
    assert_eq!(types["q"].kind, AttributeType::Number);
    assert_eq!(types["flag"].kind, AttributeType::Bool);
    assert_eq!(model::existing_tags(&txn), ["t1", "t2"]);
}

#[test]
fn a_malformed_event_and_a_dangling_member_survive_unread() {
    let doc = migrated();
    let txn = doc.transact();

    assert_eq!(model::event(&txn, MALFORMED), None);
    assert_eq!(model::catalogue(&txn, BETA).unwrap().event_count, 0);
    let events = txn.get_map(model::EVENTS).unwrap();
    assert!(yrs::Map::contains_key(&events, &txn, MALFORMED));
    let catalogues = txn.get_map(model::CATALOGUES).unwrap();
    let Some(yrs::Out::YMap(beta)) = yrs::Map::get(&catalogues, &txn, BETA) else {
        panic!("beta is missing");
    };
    let Some(yrs::Out::YMap(listed)) = yrs::Map::get(&beta, &txn, "events") else {
        panic!("beta lists nothing");
    };
    assert!(yrs::Map::contains_key(&listed, &txn, DANGLING));
}

#[test]
fn a_v2_room_is_not_migrated_again() {
    let doc = migrated();
    assert_eq!(model::migrate_v1(&doc).unwrap(), None);
    assert_eq!(model::migrate_v1(&Doc::new()).unwrap(), None);
}
