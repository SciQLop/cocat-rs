//! The rule forms the query language added: `text`, `duration`, `{"now": ...}`, `shape`,
//! and searching events without a smart catalogue.

use cocat_core::model::{self, EventRecord, NewCatalogue, Search, Vertex};
use cocat_core::rules::{self, Predicate};
use cocat_core::time::Timestamp;
use serde_json::{Map, Value, json};
use yrs::{Doc, Transact};

const SMART: &str = "5a5a5a5a-0000-4000-8000-000000000001";
const BASE: i64 = 1_738_317_600_000_000_000; // 2025-01-31 10:00:00 UTC
const SECOND: i64 = 1_000_000_000;
const DAY: i64 = 86_400;

fn at(offset: i64) -> Timestamp {
    Timestamp::from_unix_nanos(BASE + offset * SECOND).unwrap()
}

fn uuid(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

/// 1: one second, created ten days before BASE, tagged "Shock", by alice.
/// 2: one hour, created a day before BASE, rejected, source id "OMNI-7", attribute
///    `label: "Bow wave"`, with a shape.
fn room() -> Doc {
    let doc = Doc::new();
    let mut txn = doc.transact_mut();
    let mut first = EventRecord::new(uuid(1), at(0), at(1));
    first.created = Some(at(-10 * DAY));
    first.author = Some("alice".to_owned());
    first.tags = vec!["Shock".to_owned()];
    model::create_event(&mut txn, &first).unwrap();
    let mut second = EventRecord::new(uuid(2), at(100), at(3_700));
    second.created = Some(at(-DAY));
    second.status = model::Status::Rejected;
    second.source_id = Some("OMNI-7".to_owned());
    second.attributes = Map::from_iter([("label".to_owned(), json!("Bow wave"))]);
    second.shape = Some(vec![
        Vertex {
            time: at(100),
            value: 1.0,
        },
        Vertex {
            time: at(3_700),
            value: 2.0,
        },
        Vertex {
            time: at(100),
            value: 2.0,
        },
    ]);
    model::create_event(&mut txn, &second).unwrap();
    drop(txn);
    doc
}

/// The events a `filter all where predicate` rule yields, evaluated at BASE.
fn matching(predicate: Value) -> Vec<String> {
    let doc = room();
    let mut txn = doc.transact_mut();
    let new = NewCatalogue {
        uuid: SMART.to_owned(),
        name: "smart".to_owned(),
        ..NewCatalogue::default()
    };
    model::create_catalogue(&mut txn, &new).unwrap();
    let rule = json!({"filter": {"all": true}, "where": predicate});
    model::set_catalogue_rule(&mut txn, SMART, Some(&rule)).unwrap();
    let members = model::catalogue_events_at(&txn, SMART, at(0))
        .unwrap()
        .unwrap();
    members.into_iter().map(|event| event.uuid).collect()
}

fn uuids(numbers: &[u32]) -> Vec<String> {
    numbers.iter().map(|n| uuid(*n)).collect()
}

#[test]
fn text_matches_any_text_of_an_event_ignoring_case() {
    assert_eq!(matching(json!({"text": "shock"})), uuids(&[1]));
    assert_eq!(matching(json!({"text": "ALIC"})), uuids(&[1]));
    assert_eq!(matching(json!({"text": "reject"})), uuids(&[2]));
    assert_eq!(matching(json!({"text": "omni"})), uuids(&[2]));
    assert_eq!(matching(json!({"text": "bow w"})), uuids(&[2]));
    assert_eq!(matching(json!({"text": "confirmed"})), uuids(&[1]));
    assert_eq!(matching(json!({"text": "nowhere"})), uuids(&[]));
}

#[test]
fn text_begins_the_start_time() {
    assert_eq!(matching(json!({"text": "2025-01-31 10:00:0"})), uuids(&[1]));
    assert_eq!(matching(json!({"text": "2025-01-31 10:01"})), uuids(&[2]));
    assert_eq!(matching(json!({"text": "10:01"})), uuids(&[]));
}

#[test]
fn duration_is_stop_minus_start_in_seconds() {
    assert_eq!(matching(json!({"cmp": ["duration", ">", 60]})), uuids(&[2]));
    assert_eq!(
        matching(json!({"cmp": ["duration", "<=", 1.0]})),
        uuids(&[1])
    );
    assert_eq!(matching(json!({"has": "duration"})), uuids(&[1, 2]));
}

#[test]
fn now_is_the_time_the_rule_is_evaluated() {
    let recent = json!({"cmp": ["created", ">", {"now": "-7d"}]});
    assert_eq!(matching(recent), uuids(&[2]));
    let old = json!({"cmp": ["created", "<", {"now": "-1w"}]});
    assert!(rules::parse(&json!({"filter": {"all": true}, "where": old})).is_err());
    let past = json!({"cmp": ["start", "<", {"now": ""}]});
    assert_eq!(matching(past), uuids(&[]));
    let soon = json!({"cmp": ["stop", "<=", {"now": "+1h"}]});
    assert_eq!(matching(soon), uuids(&[1]));
}

#[test]
fn shape_is_a_field() {
    assert_eq!(matching(json!({"has": "shape"})), uuids(&[2]));
}

#[test]
fn the_new_forms_read_and_write_back_as_json() {
    let rule = json!({"filter": {"all": true}, "where": {"and": [
        {"text": "shock"},
        {"cmp": ["created", ">=", {"now": "-1M"}]},
        {"cmp": ["duration", "<", 10]},
    ]}});
    let parsed = rules::parse(&rule).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), rule);
}

#[test]
fn a_malformed_new_form_is_refused() {
    let refused = |predicate: Value| {
        rules::parse(&json!({"filter": {"all": true}, "where": predicate}))
            .unwrap_err()
            .to_string()
    };
    assert_eq!(
        refused(json!({"text": 5})),
        "rule.where.text: expected text, not 5"
    );
    assert_eq!(
        refused(json!({"cmp": ["created", ">", {"now": "7 days"}]})),
        "rule.where.cmp: \"7 days\" is not a time from now: write it like -7d \
         (units s, m, h, d, M, y)"
    );
}

#[test]
fn search_finds_the_events_a_predicate_matches() {
    let doc = room();
    let txn = doc.transact();
    let predicate = Predicate::Text("shock".to_owned());
    let found = model::search(&txn, &predicate, at(0)).unwrap();
    assert_eq!(
        found.into_iter().map(|e| e.uuid).collect::<Vec<_>>(),
        uuids(&[1])
    );
}

#[test]
fn a_search_tests_one_event_or_interval_at_a_time() {
    let doc = room();
    let txn = doc.transact();
    let long = rules::parse(&json!({"filter": {"all": true}, "where":
        {"cmp": ["duration", ">", 60]}}))
    .unwrap();
    let rules::Set::Filter { predicate, .. } = long else {
        panic!("not a filter")
    };
    let search = Search::new(&txn, &predicate, at(0)).unwrap();
    assert!(!search.matches(&txn, Some(&uuid(1)), 0, 0));
    assert!(search.matches(&txn, Some(&uuid(2)), 0, 0));
    assert!(search.matches(&txn, None, 0, 61 * SECOND));
    assert!(!search.matches(&txn, None, 0, 60 * SECOND));
}
