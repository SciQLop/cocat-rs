//! Searching events with a predicate, without a smart catalogue: the events of the room
//! that match, or a test of one table row at a time.

use serde_json::Value;
use yrs::ReadTxn;

use super::members::{Evaluator, item_field};
use super::read::event;
use super::{ATTRIBUTE_PREFIX, EventRecord};
use crate::rules::intervals::{Item, Source};
use crate::rules::{self, ANY_TEXT, Lookups, Predicate, RuleError};
use crate::time::Timestamp;

/// The events not in the trash that match `predicate` at `now`, in time order.
pub fn search<T: ReadTxn>(
    txn: &T,
    predicate: &Predicate,
    now: Timestamp,
) -> Result<Vec<EventRecord>, RuleError> {
    let mut items = Evaluator::new(txn, now).filter_all(predicate)?;
    items.sort();
    Ok(items
        .iter()
        .filter_map(|item| match &item.source {
            Source::Event(uuid) => event(txn, uuid),
            Source::Derived(_) => None,
        })
        .collect())
}

/// A predicate ready to test items one by one: its lookups are read once.
#[derive(Debug)]
pub struct Search {
    predicate: Predicate,
    lookups: Lookups,
}

impl Search {
    pub fn new<T: ReadTxn>(
        txn: &T,
        predicate: &Predicate,
        now: Timestamp,
    ) -> Result<Self, RuleError> {
        Ok(Self {
            predicate: predicate.clone(),
            lookups: Evaluator::new(txn, now).lookups(predicate)?,
        })
    }

    /// Whether the event `uuid` matches, or, for `None`, the derived interval
    /// `[start, stop)` (nanoseconds); an event's range is read from the room.
    pub fn matches<T: ReadTxn>(&self, txn: &T, uuid: Option<&str>, start: i64, stop: i64) -> bool {
        let item = match uuid {
            Some(uuid) => Item::event(uuid, start, stop),
            None => Item::derived(start, stop, Vec::new()),
        };
        let field = |name: &str| item_field(txn, &item, name);
        rules::test(&self.predicate, uuid, &field, &self.lookups)
    }
}

/// Whether an event read out of its room matches, as it would in the room. A term naming a
/// catalogue asks `lookups`, which knows no room: give it the answer beforehand.
pub fn record_matches(predicate: &Predicate, lookups: &Lookups, record: &EventRecord) -> bool {
    let field = |name: &str| record_field(record, name);
    rules::test(predicate, Some(&record.uuid), &field, lookups)
}

/// A field of a record as a predicate reads it in the room (see `members::event_field`).
fn record_field(record: &EventRecord, name: &str) -> Option<Value> {
    let time = |at: crate::time::Timestamp| at.unix_nanos().map(Value::from);
    let text = |text: &Option<String>| text.clone().map(Value::from);
    let number = |number: Option<f64>| number.map(Value::from);
    let list = |items: &[String]| (!items.is_empty()).then(|| Value::from(items.to_vec()));
    match name {
        "uuid" => Some(record.uuid.clone().into()),
        "start" => time(record.start),
        "stop" => time(record.stop),
        "created" => record.created.and_then(time),
        "author" => text(&record.author),
        "origin" => text(&record.origin),
        "source_id" => text(&record.source_id),
        "status" => Some(record.status.name().into()),
        "confidence" => number(record.confidence),
        "start_err" => number(record.start_err),
        "stop_err" => number(record.stop_err),
        "tags" => list(&record.tags),
        "products" => list(&record.products),
        "shape" => record.shape.as_ref().map(|_| Value::Bool(true)),
        ANY_TEXT => Some(any_text(record)),
        _ => name
            .strip_prefix(ATTRIBUTE_PREFIX)
            .and_then(|key| record.attributes.get(key).cloned()),
    }
}

/// The texts `ANY_TEXT` names (see `members::any_text`).
fn any_text(record: &EventRecord) -> Value {
    let attributes = record.attributes.values().map(|value| match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    });
    let named = [&record.author, &record.source_id, &record.origin];
    record
        .tags
        .iter()
        .cloned()
        .chain(named.into_iter().flatten().cloned())
        .chain(attributes)
        .chain([record.status.name().to_owned()])
        .map(Value::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use yrs::{Doc, Transact};

    use super::*;
    use crate::model::{Status, Vertex, create_event, members::item_field};
    use crate::time::Timestamp;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_unix_nanos(seconds * 1_000_000_000).unwrap()
    }

    fn full() -> EventRecord {
        let mut record = EventRecord::new("e1", at(10), at(70));
        record.author = Some("alice@lab.fr".into());
        record.created = Some(at(5));
        record.origin = Some("o1".into());
        record.status = Status::Reviewed;
        record.confidence = Some(0.8);
        record.start_err = Some(1.5);
        record.stop_err = Some(2.0);
        record.source_id = Some("S-1".into());
        record.tags = vec!["shock".into(), "bow".into()];
        record.products = vec!["amda/b".into()];
        record.attributes.insert("density".into(), json!(12.5));
        record.attributes.insert("spacecraft".into(), json!("MMS1"));
        record
    }

    fn shaped() -> EventRecord {
        let mut record = EventRecord::new("e2", at(0), at(0));
        record.shape = Some(vec![
            Vertex {
                time: at(1),
                value: 1.0,
            },
            Vertex {
                time: at(4),
                value: 2.0,
            },
        ]);
        record
    }

    /// Lists in one order, numbers as floats: the room may store 2.0 as 2.
    fn sorted(value: Option<Value>) -> Option<Value> {
        match value {
            Some(Value::Number(number)) => number.as_f64().map(Value::from),
            Some(Value::Array(mut items)) => {
                items.sort_by_key(|item| item.to_string());
                Some(Value::Array(items))
            }
            other => other,
        }
    }

    #[test]
    fn a_record_reads_its_fields_as_the_room_does() {
        let doc = Doc::new();
        let records = [full(), shaped(), EventRecord::new("e3", at(1), at(2))];
        {
            let mut txn = doc.transact_mut();
            for record in &records {
                create_event(&mut txn, record).unwrap();
            }
        }
        let txn = doc.transact();
        let names =
            rules::FIELDS
                .iter()
                .copied()
                .chain([ANY_TEXT, "#density", "#spacecraft", "#missing"]);
        for record in &records {
            let stored = crate::model::event(&txn, &record.uuid).unwrap();
            let item = crate::rules::Item::event(&record.uuid, 0, 0);
            for name in names.clone().filter(|name| *name != "duration") {
                let in_room = item_field(&txn, &item, name);
                let in_room = match name {
                    "shape" => in_room.map(|_| Value::Bool(true)),
                    _ => in_room,
                };
                assert_eq!(
                    sorted(record_field(&stored, name)),
                    sorted(in_room),
                    "{} {name}",
                    record.uuid
                );
            }
        }
    }
}
