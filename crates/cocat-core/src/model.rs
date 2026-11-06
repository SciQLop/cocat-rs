//! The catalogue and event maps a room document holds.
//!
//! Both roots are maps keyed by uuid string. An event keeps its times as text,
//! its tags and products as maps used as sets, and its rating as a number or
//! null; a catalogue keeps a map of the uuids it lists. Reading this layout is
//! pure: every function borrows a read transaction and returns owned values.

use serde::Serialize;
use serde_json::{Map, Value};
use tracing::warn;
use yrs::types::ToJson as _;
use yrs::{Any, Map as _, MapRef, Number, Out, ReadTxn};

use crate::time::Timestamp;

pub const CATALOGUES: &str = "catalogues";
pub const EVENTS: &str = "events";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct CatalogueSummary {
    pub uuid: String,
    pub name: String,
    pub author: String,
    pub tags: Vec<String>,
    #[schema(value_type = Object)]
    pub attributes: Map<String, Value>,
    pub event_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct EventRecord {
    pub uuid: String,
    #[schema(value_type = String)]
    pub start: Timestamp,
    #[schema(value_type = String)]
    pub stop: Timestamp,
    pub author: String,
    pub tags: Vec<String>,
    pub products: Vec<String>,
    pub rating: Option<i64>,
    #[schema(value_type = Object)]
    pub attributes: Map<String, Value>,
}

pub fn catalogues<T: ReadTxn>(txn: &T) -> Vec<CatalogueSummary> {
    let Some(root) = txn.get_map(CATALOGUES) else {
        return Vec::new();
    };
    let events = txn.get_map(EVENTS);
    let mut catalogues: Vec<CatalogueSummary> = root
        .iter(txn)
        .filter_map(|(uuid, value)| summary(txn, events.as_ref(), uuid, value))
        .collect();
    catalogues.sort_by(|left, right| {
        (left.name.as_str(), left.uuid.as_str()).cmp(&(right.name.as_str(), right.uuid.as_str()))
    });
    catalogues
}

pub fn catalogue<T: ReadTxn>(txn: &T, uuid: &str) -> Option<CatalogueSummary> {
    let root = txn.get_map(CATALOGUES)?;
    let events = txn.get_map(EVENTS);
    summary(txn, events.as_ref(), uuid, root.get(txn, uuid)?)
}

pub fn catalogue_events<T: ReadTxn>(txn: &T, uuid: &str) -> Option<Vec<EventRecord>> {
    let root = txn.get_map(CATALOGUES)?;
    let Out::YMap(catalogue) = root.get(txn, uuid)? else {
        return None;
    };
    let events = txn.get_map(EVENTS);

    let mut records: Vec<EventRecord> = membership(&catalogue, txn, "events")
        .iter()
        .filter_map(|event| {
            let map = match events.as_ref().and_then(|events| events.get(txn, event)) {
                Some(Out::YMap(map)) => map,
                _ => return None,
            };
            match record(txn, &map) {
                Some(record) => Some(record),
                None => {
                    warn!(event = %event, "skipping a malformed event");
                    None
                }
            }
        })
        .collect();
    records.sort_by(|left, right| {
        (left.start, left.uuid.as_str()).cmp(&(right.start, right.uuid.as_str()))
    });
    Some(records)
}

fn summary<T: ReadTxn>(
    txn: &T,
    events: Option<&MapRef>,
    uuid: &str,
    value: Out,
) -> Option<CatalogueSummary> {
    let Out::YMap(map) = value else {
        return None;
    };
    let listed = membership(&map, txn, "events");
    Some(CatalogueSummary {
        uuid: uuid.to_owned(),
        name: text(&map, txn, "name").unwrap_or_default(),
        author: text(&map, txn, "author").unwrap_or_default(),
        tags: set(&map, txn, "tags"),
        attributes: attributes(&map, txn),
        event_count: match events {
            Some(events) => listed
                .iter()
                .filter(|event| events.contains_key(txn, event))
                .count(),
            None => 0,
        },
    })
}

fn record<T: ReadTxn>(txn: &T, map: &MapRef) -> Option<EventRecord> {
    Some(EventRecord {
        uuid: text(map, txn, "uuid")?,
        start: Timestamp::parse(&text(map, txn, "start")?)?,
        stop: Timestamp::parse(&text(map, txn, "stop")?)?,
        author: text(map, txn, "author")?,
        tags: set(map, txn, "tags"),
        products: set(map, txn, "products"),
        rating: rating(map, txn)?,
        attributes: attributes(map, txn),
    })
}

fn rating<T: ReadTxn>(map: &MapRef, txn: &T) -> Option<Option<i64>> {
    match map.get(txn, "rating")? {
        Out::Any(Any::Null | Any::Undefined) => Some(None),
        Out::Any(Any::Number(Number::Int(value))) => Some(Some(value)),
        Out::Any(Any::Number(Number::Float(value))) if value.fract() == 0.0 => {
            Some(Some(value as i64))
        }
        _ => None,
    }
}

fn text<T: ReadTxn>(map: &MapRef, txn: &T, key: &str) -> Option<String> {
    match map.get(txn, key)? {
        Out::Any(Any::String(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn set<T: ReadTxn>(map: &MapRef, txn: &T, key: &str) -> Vec<String> {
    let mut values = membership(map, txn, key);
    values.sort();
    values
}

fn membership<T: ReadTxn>(map: &MapRef, txn: &T, key: &str) -> Vec<String> {
    match map.get(txn, key) {
        Some(Out::YMap(set)) => set.iter(txn).map(|(key, _)| key.to_owned()).collect(),
        _ => Vec::new(),
    }
}

fn attributes<T: ReadTxn>(map: &MapRef, txn: &T) -> Map<String, Value> {
    match map.get(txn, "attributes") {
        Some(Out::YMap(attributes)) => {
            let mut json = String::new();
            attributes.to_json(txn).to_json(&mut json);
            match serde_json::from_str(&json) {
                Ok(Value::Object(map)) => map,
                _ => Map::new(),
            }
        }
        _ => Map::new(),
    }
}
