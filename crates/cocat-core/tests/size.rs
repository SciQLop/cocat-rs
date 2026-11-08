//! Layout v2 must not make a hand-made event cost more than v1 did (design: "Size target").

use std::sync::Arc;

use cocat_core::model::{self, EventRecord, Origin, OriginKind};
use cocat_core::time::Timestamp;
use serde_json::{Map, Value, json};
use yrs::{Any, Doc, In, Map as _, MapPrelim, ReadTxn as _, StateVector, Transact, WriteTxn as _};

const EVENTS: usize = 1_000;

fn start(index: usize) -> Timestamp {
    let base = Timestamp::parse("2025-01-31 10:00:00").unwrap();
    let nanos = base.unix_nanos().unwrap() + index as i64 * 60_000_000_123;
    Timestamp::from_unix_nanos(nanos).unwrap()
}

fn uuid(index: usize) -> String {
    format!("{index:08x}-1111-4111-8111-111111111111")
}

fn attributes(index: usize) -> Map<String, Value> {
    match json!({"quality": index as f64 / 7.0, "region": "magnetosheath"}) {
        Value::Object(map) => map,
        _ => unreachable!(),
    }
}

fn tags() -> Vec<String> {
    vec!["shock".to_owned(), "reviewed-by-ab".to_owned()]
}

fn products() -> Vec<String> {
    vec!["amda/mms1_fgm_brst".to_owned()]
}

/// The v1 writer as Python cocat laid an event out, kept here only to measure against.
fn create_v1_event(txn: &mut yrs::TransactionMut<'_>, index: usize) {
    let set = |entries: &[String]| {
        let mut set = MapPrelim::default();
        for entry in entries {
            set.insert(Arc::from(entry.as_str()), In::from(true));
        }
        In::from(set)
    };
    let mut attributes_prelim = MapPrelim::default();
    for (key, value) in attributes(index) {
        let value: Any = serde_json::from_value(value).unwrap();
        attributes_prelim.insert(Arc::from(key.as_str()), In::from(value));
    }
    let uuid = uuid(index);
    let stop = Timestamp::from_unix_nanos(start(index).unix_nanos().unwrap() + 30_000_000_000);
    let mut content = MapPrelim::default();
    content.insert(Arc::from("uuid"), In::from(uuid.as_str()));
    content.insert(
        Arc::from("start"),
        In::from(start(index).as_text().unwrap()),
    );
    content.insert(
        Arc::from("stop"),
        In::from(stop.unwrap().as_text().unwrap()),
    );
    content.insert(Arc::from("author"), In::from("ada@example.org"));
    content.insert(Arc::from("tags"), set(&tags()));
    content.insert(Arc::from("products"), set(&products()));
    content.insert(Arc::from("rating"), In::from(Any::Null));
    content.insert(Arc::from("attributes"), In::from(attributes_prelim));
    txn.get_or_insert_map("events")
        .insert(txn, uuid.as_str(), content);
}

fn v2_event(index: usize) -> EventRecord {
    let stop = Timestamp::from_unix_nanos(start(index).unix_nanos().unwrap() + 30_000_000_000);
    let mut event = EventRecord::new(uuid(index), start(index), stop.unwrap());
    event.author = Some("ada@example.org".to_owned());
    event.created = Some(start(index));
    event.tags = tags();
    event.products = products();
    event.attributes = attributes(index);
    event
}

fn bytes(doc: &Doc) -> usize {
    doc.transact()
        .encode_state_as_update_v1(&StateVector::default())
        .len()
}

#[test]
fn a_hand_made_v2_event_costs_at_most_ten_percent_more_than_v1() {
    let v1 = Doc::new();
    let v2 = Doc::new();
    for index in 0..EVENTS {
        create_v1_event(&mut v1.transact_mut(), index);
        model::create_event(&mut v2.transact_mut(), &v2_event(index)).unwrap();
    }

    let (v1_bytes, v2_bytes) = (bytes(&v1), bytes(&v2));
    println!(
        "{EVENTS} hand-made events: v1 {v1_bytes} bytes ({} per event), v2 {v2_bytes} bytes ({} per event)",
        v1_bytes / EVENTS,
        v2_bytes / EVENTS
    );
    assert!(
        v2_bytes as f64 <= 1.10 * v1_bytes as f64,
        "v2 {v2_bytes} bytes against v1 {v1_bytes}"
    );

    let models = Doc::new();
    let origin = Origin {
        id: "run-1".to_owned(),
        kind: OriginKind::Model,
        name: "shock finder".to_owned(),
        version: None,
        author: None,
        created: None,
        used: Vec::new(),
        parameters: None,
        derived_from: None,
    };
    model::create_origin(&mut models.transact_mut(), &origin).unwrap();
    let empty = bytes(&models);
    for index in 0..EVENTS {
        let stop = Timestamp::from_unix_nanos(start(index).unix_nanos().unwrap() + 30_000_000_000);
        let mut event = EventRecord::new(uuid(index), start(index), stop.unwrap());
        event.origin = Some("run-1".to_owned());
        event.confidence = Some(0.8);
        model::create_event(&mut models.transact_mut(), &event).unwrap();
    }
    println!(
        "a model event (origin, confidence, no author): {} bytes",
        (bytes(&models) - empty) / EVENTS
    );
}
