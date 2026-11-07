//! Evaluating a smart catalogue over a 100k-event room (design section 3: a filter in under
//! 100 ms in release). Debug builds only print, so a debug run never flakes.

use std::time::{Duration, Instant};

use cocat_core::model::{self, EventRecord, Member, NewCatalogue};
use cocat_core::time::Timestamp;
use serde_json::{Value, json};
use yrs::{Doc, Transact};

const EVENTS: i64 = 100_000;
const FILTER: &str = "f0000000-0000-4000-8000-000000000001";
const MERGE: &str = "f0000000-0000-4000-8000-000000000002";

/// Events a minute apart, 30 s long, with an author, two tags and two attributes.
fn room() -> Doc {
    let base = Timestamp::parse("2025-01-01 00:00:00")
        .and_then(Timestamp::unix_nanos)
        .unwrap();
    let at = |seconds: i64| Timestamp::from_unix_nanos(base + seconds * 1_000_000_000).unwrap();
    let doc = Doc::new();
    let mut txn = doc.transact_mut();
    for n in 0..EVENTS {
        let uuid = format!("00000000-0000-4000-8000-{n:012}");
        let mut event = EventRecord::new(uuid, at(n * 60), at(n * 60 + 30));
        event.author = Some("bench@example.org".to_owned());
        event.tags = vec!["shock".to_owned(), format!("orbit-{}", n / 1000)];
        let Value::Object(attributes) = json!({"q": n % 100, "region": "magnetosheath"}) else {
            unreachable!()
        };
        event.attributes = attributes;
        model::create_event(&mut txn, &event).unwrap();
    }
    for (uuid, rule) in [
        (
            FILTER,
            json!({"filter": {"all": true}, "where": {"cmp": ["#q", "<", 10]}}),
        ),
        (MERGE, json!({"merge": {"all": true}, "gap": 30})),
    ] {
        let catalogue = NewCatalogue {
            uuid: uuid.to_owned(),
            name: uuid.to_owned(),
            ..NewCatalogue::default()
        };
        model::create_catalogue(&mut txn, &catalogue).unwrap();
        model::set_catalogue_rule(&mut txn, uuid, Some(&rule)).unwrap();
    }
    drop(txn);
    doc
}

fn timed(doc: &Doc, uuid: &str) -> (Vec<Member>, Duration) {
    let txn = doc.transact();
    let started = Instant::now();
    let members = model::catalogue_members(&txn, uuid).unwrap().unwrap();
    (members, started.elapsed())
}

#[test]
fn a_filter_and_a_merge_over_100k_events() {
    let doc = room();

    let (filtered, filter_time) = timed(&doc, FILTER);
    let (merged, merge_time) = timed(&doc, MERGE);
    println!(
        "100k events: filter {filter_time:?} ({} members)",
        filtered.len()
    );
    println!(
        "100k events: merge {merge_time:?} ({} members)",
        merged.len()
    );

    assert_eq!(filtered.len(), 10_000);
    let [Member::Derived { sources, .. }] = merged.as_slice() else {
        panic!("the merge should make one interval")
    };
    assert_eq!(sources.len(), 100_000);
    #[cfg(not(debug_assertions))]
    assert!(
        filter_time < Duration::from_millis(100),
        "the filter took {filter_time:?}"
    );
}
