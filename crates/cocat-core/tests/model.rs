//! Reading and writing the catalogue and event maps a room document holds.

use std::fs;

use cocat_core::model::{self, NewCatalogue, NewEvent};
use cocat_core::store::FileRoomStore;
use cocat_core::time::Timestamp;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tempfile::TempDir;
use yrs::updates::decoder::Decode;
use yrs::{Any, Doc, Map as _, Number, Out, ReadTxn as _, Transact, Update};

const FIXTURE: &[u8] = include_bytes!("fixtures/catalogues.y");
const EXPECTED: &str = include_str!("fixtures/catalogues.expected.json");
const ALPHA: &str = "6f9c1a52-0b0d-4f4e-9d5e-1b2c3d4e5f60";
const BETA: &str = "0e4d2c31-5a6b-4c7d-8e9f-0a1b2c3d4e5f";
const MALFORMED: &str = "11111111-2222-4333-8444-555555555555";
const EVENT: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const START: &str = "2025-01-31 10:00:00";
const STOP: &str = "2025-01-31 10:01:00";

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

fn attributes(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

fn event_map(doc: &Doc, uuid: &str) -> yrs::MapRef {
    let txn = doc.transact();
    let events = txn.get_map(model::EVENTS).unwrap();
    match events.get(&txn, uuid) {
        Some(Out::YMap(map)) => map,
        other => panic!("event {uuid} is {other:?}"),
    }
}

fn field(doc: &Doc, uuid: &str, key: &str) -> Out {
    let txn = doc.transact();
    event_map(doc, uuid).get(&txn, key).unwrap()
}

/// A room holding one catalogue that lists one event, written the way the client writes it.
fn written() -> Doc {
    let doc = Doc::new();
    let tags = vec!["t1".to_owned(), "t2".to_owned()];
    let event_tags = vec!["t1".to_owned()];
    let products = vec!["p1".to_owned()];
    let catalogue_attributes = attributes(json!({"mission": "MMS", "level": 2}));
    let event_attributes = attributes(json!({"q": 1.5, "flag": true}));
    {
        let mut txn = doc.transact_mut();
        model::create_catalogue(
            &mut txn,
            &NewCatalogue {
                uuid: ALPHA,
                name: "alpha",
                author: "a@x.org",
                tags: &tags,
                attributes: &catalogue_attributes,
            },
        )
        .unwrap();
        model::create_event(
            &mut txn,
            &NewEvent {
                uuid: EVENT,
                start: Timestamp::parse(START).unwrap(),
                stop: Timestamp::parse(STOP).unwrap(),
                author: "a@x.org",
                tags: &event_tags,
                products: &products,
                rating: Some(3),
                attributes: &event_attributes,
            },
        )
        .unwrap();
        assert!(model::add_catalogue_events(
            &mut txn,
            ALPHA,
            &[EVENT.to_owned()]
        ));
    }
    doc
}

#[test]
fn writes_what_the_reader_reads() {
    let doc = written();
    let txn = doc.transact();

    let catalogue = model::catalogue(&txn, ALPHA).unwrap();
    assert_eq!(catalogue.name, "alpha");
    assert_eq!(catalogue.author, "a@x.org");
    let tags: Vec<&str> = catalogue.tags.iter().map(String::as_str).collect();
    assert_eq!(tags, ["t1", "t2"]);
    assert_eq!(
        catalogue.attributes,
        attributes(json!({"mission": "MMS", "level": 2}))
    );
    assert_eq!(catalogue.event_count, 1);

    let events = model::catalogue_events(&txn, ALPHA).unwrap();
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.uuid, EVENT);
    assert_eq!(event.start, Timestamp::parse(START).unwrap());
    assert_eq!(event.stop, Timestamp::parse(STOP).unwrap());
    assert_eq!(event.author, "a@x.org");
    assert_eq!(event.rating, Some(3));
    let event_tags: Vec<&str> = event.tags.iter().map(String::as_str).collect();
    assert_eq!(event_tags, ["t1"]);
    assert_eq!(event.products, ["p1"]);
    assert_eq!(
        event.attributes,
        attributes(json!({"q": 1.5, "flag": true}))
    );
}

#[test]
fn writes_tags_and_ratings_in_the_shape_python_uses() {
    let doc = written();
    {
        let txn = doc.transact();
        let event = event_map(&doc, EVENT);
        assert!(
            matches!(event.get(&txn, "tags"), Some(Out::YMap(_))),
            "tags are a map used as a set"
        );
        assert_eq!(
            event.get(&txn, "rating"),
            Some(Out::Any(Any::Number(Number::Int(3))))
        );
    }

    let mut txn = doc.transact_mut();
    model::create_event(
        &mut txn,
        &NewEvent {
            uuid: BETA,
            start: Timestamp::parse("2025-01-31 11:00:00.123456").unwrap(),
            stop: Timestamp::parse("2025-01-31 11:01:00.654321").unwrap(),
            author: "a@x.org",
            tags: &[],
            products: &[],
            rating: None,
            attributes: &Map::new(),
        },
    )
    .unwrap();
    drop(txn);

    assert_eq!(
        field(&doc, BETA, "rating"),
        Out::Any(Any::Null),
        "no rating is written as an explicit null"
    );
    assert_eq!(
        field(&doc, EVENT, "start"),
        Out::Any(Any::String("2025-01-31 10:00:00".into()))
    );
    assert_eq!(
        field(&doc, BETA, "start"),
        Out::Any(Any::String("2025-01-31 11:00:00.123456".into()))
    );
    assert_eq!(
        field(&doc, BETA, "stop"),
        Out::Any(Any::String("2025-01-31 11:01:00.654321".into()))
    );
}

#[test]
fn renames_a_catalogue() {
    let doc = written();
    assert!(model::rename_catalogue(
        &mut doc.transact_mut(),
        ALPHA,
        "renamed"
    ));

    let txn = doc.transact();
    assert_eq!(model::catalogue(&txn, ALPHA).unwrap().name, "renamed");
}

#[test]
fn merges_and_removes_attributes() {
    let doc = written();
    {
        let mut txn = doc.transact_mut();
        let merged = attributes(json!({"mission": "Cluster", "level": 3}));
        assert!(model::set_catalogue_attributes(&mut txn, ALPHA, &merged).unwrap());
        assert!(model::remove_catalogue_attributes(
            &mut txn,
            ALPHA,
            &["level".to_owned()]
        ));

        let event = attributes(json!({"note": "ok"}));
        assert!(model::set_event_attributes(&mut txn, EVENT, &event).unwrap());
        assert!(model::remove_event_attributes(
            &mut txn,
            EVENT,
            &["q".to_owned()]
        ));
    }

    let txn = doc.transact();
    assert_eq!(
        model::catalogue(&txn, ALPHA).unwrap().attributes,
        attributes(json!({"mission": "Cluster"}))
    );
    let event = &model::catalogue_events(&txn, ALPHA).unwrap()[0];
    assert_eq!(
        event.attributes,
        attributes(json!({"flag": true, "note": "ok"}))
    );
}

#[test]
fn adds_and_removes_events() {
    let doc = written();
    {
        let mut txn = doc.transact_mut();
        assert!(model::remove_catalogue_events(
            &mut txn,
            ALPHA,
            &[EVENT.to_owned()]
        ));
    }
    let txn = doc.transact();
    assert_eq!(model::catalogue(&txn, ALPHA).unwrap().event_count, 0);
    assert_eq!(
        txn.get_map(model::EVENTS).unwrap().len(&txn),
        1,
        "leaving the catalogue leaves the event in the room"
    );
    drop(txn);

    {
        let mut txn = doc.transact_mut();
        assert!(model::add_catalogue_events(
            &mut txn,
            ALPHA,
            &[EVENT.to_owned()]
        ));
    }
    let txn = doc.transact();
    assert_eq!(model::catalogue_events(&txn, ALPHA).unwrap().len(), 1);
}

fn listed_events(doc: &Doc, uuid: &str) -> u32 {
    let txn = doc.transact();
    let catalogues = txn.get_map(model::CATALOGUES).unwrap();
    let Some(Out::YMap(catalogue)) = catalogues.get(&txn, uuid) else {
        panic!("catalogue {uuid} is missing");
    };
    match catalogue.get(&txn, "events") {
        Some(Out::YMap(listed)) => listed.len(&txn),
        other => panic!("events are {other:?}"),
    }
}

#[test]
fn deleting_an_event_removes_it_from_every_catalogue() {
    let doc = written();
    let empty = Map::new();
    {
        let mut txn = doc.transact_mut();
        model::create_catalogue(
            &mut txn,
            &NewCatalogue {
                uuid: BETA,
                name: "beta",
                author: "b@x.org",
                tags: &[],
                attributes: &empty,
            },
        )
        .unwrap();
        assert!(model::add_catalogue_events(
            &mut txn,
            BETA,
            &[EVENT.to_owned()]
        ));
        assert!(model::delete_event(&mut txn, EVENT));
        assert!(!model::delete_event(&mut txn, EVENT));
    }

    let txn = doc.transact();
    assert_eq!(txn.get_map(model::EVENTS).unwrap().len(&txn), 0);
    drop(txn);
    assert_eq!(listed_events(&doc, ALPHA), 0);
    assert_eq!(listed_events(&doc, BETA), 0);
    let txn = doc.transact();
    assert_eq!(model::catalogue_events(&txn, ALPHA), Some(Vec::new()));
}

#[test]
fn deleting_a_catalogue_keeps_its_events() {
    let doc = written();
    assert!(model::delete_catalogue(&mut doc.transact_mut(), ALPHA));

    let txn = doc.transact();
    assert_eq!(model::catalogue(&txn, ALPHA), None);
    assert_eq!(txn.get_map(model::EVENTS).unwrap().len(&txn), 1);
}

#[test]
fn sets_an_event_range() {
    let doc = written();
    let start = Timestamp::parse("2025-02-01 00:00:00.000001").unwrap();
    let stop = Timestamp::parse("2025-02-01 00:01:00").unwrap();
    assert!(model::set_event_range(&mut doc.transact_mut(), EVENT, start, stop).unwrap());

    let txn = doc.transact();
    let event = &model::catalogue_events(&txn, ALPHA).unwrap()[0];
    assert_eq!((event.start, event.stop), (start, stop));
}

#[test]
fn writing_to_an_unknown_object_changes_nothing() {
    let doc = written();
    let unknown = "99999999-8888-4777-8666-555555555555";
    let empty = Map::new();
    {
        let mut txn = doc.transact_mut();
        assert!(!model::rename_catalogue(&mut txn, unknown, "nothing"));
        assert!(!model::delete_catalogue(&mut txn, unknown));
        assert!(!model::delete_event(&mut txn, unknown));
        assert!(!model::set_catalogue_attributes(&mut txn, unknown, &empty).unwrap());
        assert!(!model::remove_catalogue_attributes(&mut txn, unknown, &[]));
        assert!(!model::set_event_attributes(&mut txn, unknown, &empty).unwrap());
        assert!(!model::remove_event_attributes(&mut txn, unknown, &[]));
        assert!(!model::add_catalogue_events(&mut txn, unknown, &[]));
        assert!(!model::remove_catalogue_events(&mut txn, unknown, &[]));
        let now = Timestamp::parse(START).unwrap();
        assert!(!model::set_event_range(&mut txn, unknown, now, now).unwrap());
    }

    let txn = doc.transact();
    assert_eq!(model::catalogues(&txn).len(), 1);
    assert_eq!(txn.get_map(model::EVENTS).unwrap().len(&txn), 1);
}

#[test]
fn a_rust_update_leaves_the_python_fixture_alone() {
    let empty = Map::new();
    let update = {
        let doc = Doc::new();
        let mut txn = doc.transact_mut();
        model::create_catalogue(
            &mut txn,
            &NewCatalogue {
                uuid: "22222222-3333-4444-8555-666666666666",
                name: "written-by-rust",
                author: "rust@x.org",
                tags: &["rust".to_owned()],
                attributes: &empty,
            },
        )
        .unwrap();
        txn.encode_update_v1()
    };

    let doc = document();
    doc.transact_mut()
        .apply_update(Update::decode_v1(&update).unwrap())
        .unwrap();
    let txn = doc.transact();

    let catalogues = model::catalogues(&txn);
    let names: Vec<&str> = catalogues
        .iter()
        .map(|catalogue| catalogue.name.as_str())
        .collect();
    assert_eq!(names, ["alpha", "beta", "written-by-rust"]);
    assert_eq!(model::catalogue_events(&txn, ALPHA).unwrap().len(), 3);
    assert_eq!(
        model::catalogue(&txn, "22222222-3333-4444-8555-666666666666")
            .map(|catalogue| catalogue.tags),
        Some(vec!["rust".to_owned()])
    );
}
