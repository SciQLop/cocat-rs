//! A small room for the tests: three events, one in the trash, and four catalogues.

use cocat_core::model::{self, EventRecord, NewCatalogue, Origin, OriginKind, Status};
use cocat_core::time::Timestamp;
use serde_json::{Value, json};
use yrs::{Doc, Transact, TransactionMut};

/// 2019-11-03 00:00:00 UTC.
pub const DAY0: i64 = 1_572_739_200 * SECOND;
pub const SECOND: i64 = 1_000_000_000;
pub const DAY: i64 = 86_400 * SECOND;

pub fn at(nanos: i64) -> Timestamp {
    Timestamp::from_unix_nanos(nanos).unwrap()
}

struct Spec {
    uuid: &'static str,
    start: i64,
    seconds: i64,
    tags: &'static [&'static str],
    confidence: f64,
    status: Status,
    author: Option<&'static str>,
    origin: Option<&'static str>,
    attributes: Value,
}

fn record(spec: Spec) -> EventRecord {
    let mut record = EventRecord::new(
        spec.uuid,
        at(spec.start),
        at(spec.start + spec.seconds * SECOND),
    );
    record.tags = spec.tags.iter().map(|tag| (*tag).to_owned()).collect();
    record.confidence = Some(spec.confidence);
    record.status = spec.status;
    record.author = spec.author.map(str::to_owned);
    record.origin = spec.origin.map(str::to_owned);
    record.start_err = Some(0.5);
    if let Value::Object(attributes) = spec.attributes {
        record.attributes = attributes;
    }
    record
}

fn events() -> [EventRecord; 4] {
    [
        record(Spec {
            uuid: "e1",
            start: DAY0 + (14 * 3600 + 2 * 60 + 11) * SECOND,
            seconds: 41,
            tags: &["sw", "sheath"],
            confidence: 0.82,
            status: Status::Confirmed,
            author: Some("alexis.jeandet"),
            origin: None,
            attributes: json!({"spacecraft": "MMS1", "amplitude": 2.4}),
        }),
        record(Spec {
            uuid: "e2",
            start: DAY0 + 2 * DAY + (8 * 3600 + 47 * 60 + 30) * SECOND,
            seconds: 12,
            tags: &["sw"],
            confidence: 0.74,
            status: Status::Preliminary,
            author: None,
            origin: Some("o1"),
            attributes: json!({"spacecraft": "MMS2"}),
        }),
        record(Spec {
            uuid: "e3",
            start: DAY0 + 6 * DAY + (21 * 3600 + 15 * 60 + 2) * SECOND,
            seconds: 63,
            tags: &["sw", "burst"],
            confidence: 0.95,
            status: Status::Final,
            author: Some("b.lavraud"),
            origin: None,
            attributes: json!({"region": "magnetosheath"}),
        }),
        record(Spec {
            uuid: "e4",
            start: DAY0 + 3 * DAY,
            seconds: 30,
            tags: &["sheath"],
            confidence: 0.5,
            status: Status::Confirmed,
            author: Some("n.dubois"),
            origin: None,
            attributes: json!({}),
        }),
    ]
}

fn catalogue(txn: &mut TransactionMut<'_>, uuid: &str, name: &str, rule: Option<Value>) {
    let catalogue = NewCatalogue {
        uuid: uuid.into(),
        name: name.into(),
        ..NewCatalogue::default()
    };
    model::create_catalogue(txn, &catalogue).unwrap();
    if let Some(rule) = rule {
        model::set_catalogue_rule(txn, uuid, Some(&rule)).unwrap();
    }
}

pub fn room() -> Doc {
    let doc = Doc::new();
    {
        let mut txn = doc.transact_mut();
        let finder = Origin {
            id: "o1".into(),
            kind: OriginKind::Detector,
            name: "sw-finder".into(),
            version: Some("1.2".into()),
            author: None,
            created: None,
            used: Vec::new(),
            parameters: None,
            derived_from: None,
        };
        model::create_origin(&mut txn, &finder).unwrap();
        for event in events() {
            model::create_event(&mut txn, &event).unwrap();
        }
        model::set_comment(&mut txn, "e2", Some("Seen by the finder only"));
        model::delete_event(&mut txn, "e4");
        catalogue(&mut txn, "c-static", "Bow shock crossings", None);
        let listed = ["e1".into(), "e3".into(), "e4".into()];
        model::add_catalogue_events(&mut txn, "c-static", &listed);
        let sw = json!({"filter": {"all": true}, "where": {"contains": ["tags", "sw"]}});
        catalogue(&mut txn, "c-smart", "Solitary waves", Some(sw.clone()));
        let merged = json!({"merge": sw, "gap": 3 * 86_400});
        catalogue(&mut txn, "c-merged", "Wave trains", Some(merged));
        catalogue(
            &mut txn,
            "c-loop",
            "Loop",
            Some(json!({"catalogue": "c-loop"})),
        );
    }
    doc
}
