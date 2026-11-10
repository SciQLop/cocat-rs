//! Making a catalogue hold exactly the events a provider publishes.

use cocat_core::model::{
    Counts, EventRecord, NewCatalogue, Status, Wanted, add_catalogue_events, catalogue,
    create_catalogue, create_event, event, hold, listed_events,
};
use cocat_core::time::Timestamp;
use serde_json::json;
use yrs::updates::decoder::Decode;
use yrs::{Doc, ReadTxn, Transact, Update};

const CATALOGUE: &str = "c0000000-0000-4000-8000-000000000001";

fn record(uuid: &str, start: i64) -> EventRecord {
    let at = |nanos| Timestamp::from_unix_nanos(nanos).unwrap();
    let mut record = EventRecord::new(uuid, at(start), at(start + 1_000));
    record.attributes.insert("speed".into(), json!(400));
    record.attributes.insert("ratio".into(), json!(1.5));
    record
}

fn wanted(name: &str, events: Vec<EventRecord>) -> Wanted {
    Wanted {
        catalogue: CATALOGUE.to_owned(),
        name: name.to_owned(),
        events,
    }
}

fn run(doc: &Doc, wanted: &Wanted) -> Counts {
    hold(&mut doc.transact_mut(), wanted, |_| false)
}

fn live(doc: &Doc) -> Vec<String> {
    let txn = doc.transact();
    listed_events(&txn, CATALOGUE)
        .into_iter()
        .filter(|uuid| event(&txn, uuid).unwrap().status != Status::Deleted)
        .collect()
}

#[test]
fn an_empty_project_gets_the_catalogue_and_its_events() {
    let doc = Doc::new();

    let counts = run(
        &doc,
        &wanted("Flares", vec![record("a", 0), record("b", 5_000)]),
    );

    assert_eq!(
        counts,
        Counts {
            added: 2,
            ..Counts::default()
        }
    );
    assert_eq!(live(&doc), ["a", "b"]);
    assert_eq!(
        catalogue(&doc.transact(), CATALOGUE).unwrap().name,
        "Flares"
    );
    assert_eq!(
        event(&doc.transact(), "a").unwrap().attributes["speed"],
        400
    );
}

#[test]
fn holding_the_same_events_again_changes_nothing() {
    let doc = Doc::new();
    let events = wanted("Flares", vec![record("a", 0), record("b", 5_000)]);
    run(&doc, &events);
    let before = doc.transact().state_vector();

    let mut txn = doc.transact_mut();
    let counts = hold(&mut txn, &events, |_| false);
    let update = Update::decode_v1(&txn.encode_update_v1()).unwrap();
    drop(txn);

    assert!(counts.is_empty());
    assert!(
        update.state_vector().is_empty(),
        "the update carries no new block"
    );
    assert_eq!(doc.transact().state_vector(), before);
}

#[test]
fn an_event_gone_from_the_list_goes_to_the_trash() {
    let doc = Doc::new();
    run(
        &doc,
        &wanted("Flares", vec![record("a", 0), record("b", 5_000)]),
    );

    let counts = run(&doc, &wanted("Flares", vec![record("a", 0)]));

    assert_eq!(
        counts,
        Counts {
            removed: 1,
            ..Counts::default()
        }
    );
    assert_eq!(live(&doc), ["a"]);
    assert_eq!(event(&doc.transact(), "b").unwrap().status, Status::Deleted);
}

#[test]
fn an_event_back_in_the_list_comes_back_from_the_trash() {
    let doc = Doc::new();
    run(
        &doc,
        &wanted("Flares", vec![record("a", 0), record("b", 5_000)]),
    );
    run(&doc, &wanted("Flares", vec![record("a", 0)]));

    let counts = run(
        &doc,
        &wanted("Flares", vec![record("a", 0), record("b", 5_000)]),
    );

    assert_eq!(
        counts,
        Counts {
            restored: 1,
            ..Counts::default()
        }
    );
    assert_eq!(live(&doc), ["a", "b"]);
}

#[test]
fn a_corrected_event_is_rewritten() {
    let doc = Doc::new();
    run(&doc, &wanted("Flares", vec![record("a", 0)]));
    let mut fixed = record("a", 2_000);
    fixed.attributes.clear();
    fixed.attributes.insert("class".into(), json!("X1"));

    let counts = run(&doc, &wanted("Flares", vec![fixed.clone()]));

    assert_eq!(
        counts,
        Counts {
            changed: 1,
            ..Counts::default()
        }
    );
    let held = event(&doc.transact(), "a").unwrap();
    assert_eq!((held.start, held.stop), (fixed.start, fixed.stop));
    assert_eq!(held.attributes, fixed.attributes);
}

#[test]
fn a_kept_event_is_not_trashed() {
    let doc = Doc::new();
    run(
        &doc,
        &wanted("Flares", vec![record("a", 0), record("old", 9_000)]),
    );

    let counts = hold(
        &mut doc.transact_mut(),
        &wanted("Flares", vec![record("a", 0)]),
        |event| event.uuid == "old",
    );

    assert!(counts.is_empty());
    assert_eq!(live(&doc), ["a", "old"]);
}

#[test]
fn the_catalogue_follows_its_name() {
    let doc = Doc::new();
    run(&doc, &wanted("Flares", vec![record("a", 0)]));

    run(&doc, &wanted("Solar flares", vec![record("a", 0)]));

    assert_eq!(
        catalogue(&doc.transact(), CATALOGUE).unwrap().name,
        "Solar flares"
    );
}

#[test]
fn other_catalogues_and_their_events_stay_as_they_are() {
    let doc = Doc::new();
    {
        let mut txn = doc.transact_mut();
        let other = NewCatalogue {
            uuid: "other".to_owned(),
            name: "Other".to_owned(),
            ..NewCatalogue::default()
        };
        create_catalogue(&mut txn, &other).unwrap();
        create_event(&mut txn, &record("x", 0)).unwrap();
        add_catalogue_events(&mut txn, "other", &["x".to_owned()]);
    }

    run(&doc, &wanted("Flares", vec![record("a", 0)]));
    run(&doc, &wanted("Flares", vec![]));

    assert_eq!(listed_events(&doc.transact(), "other"), ["x"]);
    assert_eq!(
        event(&doc.transact(), "x").unwrap().status,
        Status::Confirmed
    );
}

#[test]
fn a_big_feed_is_held_quickly() {
    let doc = Doc::new();
    let events: Vec<EventRecord> = (0..20_000)
        .map(|n| record(&format!("e{n}"), n * 10_000))
        .collect();
    let feed = wanted("Flares", events);
    let started = std::time::Instant::now();

    let counts = run(&doc, &feed);
    let again = run(&doc, &feed);

    assert_eq!(counts.added, 20_000);
    assert!(again.is_empty());
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

#[test]
fn a_duplicated_uuid_keeps_the_last_one_and_holds_steadily() {
    let doc = Doc::new();
    let feed = wanted("Flares", vec![record("a", 0), record("a", 2_000)]);

    let counts = run(&doc, &feed);

    assert_eq!(
        counts,
        Counts {
            added: 1,
            ..Counts::default()
        }
    );
    assert_eq!(
        event(&doc.transact(), "a").unwrap().start,
        record("a", 2_000).start
    );
    assert!(run(&doc, &feed).is_empty());
}

#[test]
fn an_event_the_room_cannot_hold_is_counted_as_skipped() {
    let doc = Doc::new();
    let mut far = record("far", 0);
    far.stop = Timestamp::from_unix_micros(100_000_000_000_000_000).unwrap();

    let counts = run(&doc, &wanted("Flares", vec![record("a", 0), far.clone()]));

    assert_eq!(
        counts,
        Counts {
            added: 1,
            skipped: 1,
            ..Counts::default()
        }
    );
    assert_eq!(live(&doc), ["a"]);
    assert!(event(&doc.transact(), "far").is_none());
}

#[test]
fn a_rewrite_the_room_refuses_is_skipped_not_changed() {
    let doc = Doc::new();
    run(&doc, &wanted("Flares", vec![record("a", 0)]));
    let mut far = record("a", 0);
    far.stop = Timestamp::from_unix_micros(100_000_000_000_000_000).unwrap();

    let counts = run(&doc, &wanted("Flares", vec![far]));

    assert_eq!(
        counts,
        Counts {
            skipped: 1,
            ..Counts::default()
        }
    );
    assert_eq!(
        event(&doc.transact(), "a").unwrap().stop,
        record("a", 0).stop
    );
    assert_eq!(
        event(&doc.transact(), "a").unwrap().attributes["speed"],
        400
    );
}
