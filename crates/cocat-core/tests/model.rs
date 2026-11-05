//! Reading the catalogue and event maps cocat writes into a room document.

use std::fs;

use cocat_core::model;
use cocat_core::store::FileRoomStore;
use cocat_core::time::Timestamp;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tempfile::TempDir;
use yrs::updates::decoder::Decode;
use yrs::{Doc, Transact, Update};

const FIXTURE: &[u8] = include_bytes!("fixtures/catalogues.y");
const EXPECTED: &str = include_str!("fixtures/catalogues.expected.json");
const ALPHA: &str = "6f9c1a52-0b0d-4f4e-9d5e-1b2c3d4e5f60";
const BETA: &str = "0e4d2c31-5a6b-4c7d-8e9f-0a1b2c3d4e5f";
const MALFORMED: &str = "11111111-2222-4333-8444-555555555555";

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

fn expected() -> Expected {
    serde_json::from_str(EXPECTED).unwrap()
}

/// pycrdt hands Python every number as a float, so an integer in the document
/// reads as `2.0` in the JSON Python prints for the same value. Compare numbers
/// by value and everything else exactly.
fn same(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => left.as_f64() == right.as_f64(),
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len() && left.iter().zip(right).all(|(one, other)| same(one, other))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .all(|(key, one)| right.get(key).is_some_and(|other| same(one, other)))
        }
        _ => left == right,
    }
}

fn document() -> Doc {
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

#[test]
fn lists_catalogues_sorted_by_name() {
    let doc = document();
    let txn = doc.transact();
    let catalogues = model::catalogues(&txn);
    let expected = expected();

    let names: Vec<&str> = catalogues.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["alpha", "beta"]);
    assert_eq!(catalogues.len(), expected.catalogues.len());
    for (summary, want) in catalogues.iter().zip(&expected.catalogues) {
        assert_eq!(summary.uuid, want.uuid);
        assert_eq!(summary.name, want.name);
        assert_eq!(summary.author, want.author);
        assert_eq!(summary.tags, want.tags);
        assert!(
            same(
                &Value::Object(summary.attributes.clone()),
                &json!(want.attributes)
            ),
            "{} attributes read as {:?}",
            summary.name,
            summary.attributes
        );
    }
    assert_eq!(catalogues[0].event_count, 3);
    assert_eq!(model::catalogue(&txn, ALPHA), Some(catalogues[0].clone()));
    assert_eq!(
        model::catalogue(&txn, BETA).map(|beta| beta.tags),
        Some(vec![])
    );
}

#[test]
fn dangling_event_is_not_counted() {
    let doc = document();
    let txn = doc.transact();

    let beta = model::catalogue(&txn, BETA).unwrap();
    assert_eq!(beta.event_count, 0);
    assert_eq!(model::catalogue_events(&txn, BETA), Some(Vec::new()));
}

#[test]
fn events_match_python() {
    let doc = document();
    let txn = doc.transact();
    let events = model::catalogue_events(&txn, ALPHA).unwrap();
    let expected = expected();

    assert_eq!(events.len(), expected.events.len());
    for (event, want) in events.iter().zip(&expected.events) {
        assert_eq!(event.uuid, want.uuid);
        assert_eq!(event.author, want.author);
        assert_eq!(event.tags, want.tags);
        assert_eq!(event.products, want.products);
        assert_eq!(event.rating, want.rating.map(|rating| rating as i64));
        assert_eq!(
            Value::Object(event.attributes.clone()),
            json!(want.attributes)
        );
        assert_eq!(event.start, Timestamp::parse(&want.start).unwrap());
        assert_eq!(event.stop, Timestamp::parse(&want.stop).unwrap());
    }
}

#[test]
fn malformed_event_is_skipped() {
    let doc = document();
    let txn = doc.transact();

    let mut seen: Vec<String> = model::catalogues(&txn)
        .iter()
        .flat_map(|catalogue| model::catalogue_events(&txn, &catalogue.uuid).unwrap())
        .map(|event| event.uuid)
        .collect();
    seen.sort();
    assert!(!seen.contains(&MALFORMED.to_owned()), "read {seen:?}");
    assert_eq!(seen.len(), 3);
}

#[test]
fn unknown_catalogue_is_none() {
    let doc = document();
    let txn = doc.transact();

    assert_eq!(model::catalogue(&txn, "no-such-room"), None);
    assert_eq!(model::catalogue_events(&txn, "no-such-room"), None);
}

#[test]
fn an_empty_document_has_no_catalogues() {
    let doc = Doc::new();
    let txn = doc.transact();

    assert_eq!(model::catalogues(&txn), Vec::new());
    assert_eq!(model::catalogue(&txn, ALPHA), None);
}

#[test]
fn times_parse_as_python_writes_them() {
    let utc = Timestamp::parse("2025-01-31 00:00:00").unwrap();
    assert_eq!(utc.to_string(), "2025-01-31T00:00:00.000000Z");
    assert_eq!(Timestamp::parse("2025-01-31 00:00:00+00:00"), Some(utc));

    let precise = Timestamp::parse("2025-01-31T01:02:03.004500Z").unwrap();
    assert_eq!(precise.to_string(), "2025-01-31T01:02:03.004500Z");

    let offset = Timestamp::parse("2025-01-31 02:00:00+02:00").unwrap();
    assert_eq!(offset, utc);
    assert_eq!(offset.to_string(), "2025-01-31T00:00:00.000000Z");
    assert_eq!(utc.unix_micros(), 1_738_281_600_000_000);
}

#[test]
fn times_that_are_not_times_do_not_parse() {
    assert_eq!(Timestamp::parse("2025-13-01 00:00:00"), None);
    assert_eq!(Timestamp::parse(""), None);
    assert_eq!(Timestamp::parse("not a time"), None);
}

#[test]
fn timestamps_order_by_instant() {
    let early = Timestamp::parse("2025-01-31 09:00:00").unwrap();
    let late = Timestamp::parse("2025-01-31 12:00:00+02:00").unwrap();

    assert!(early < late);
}
