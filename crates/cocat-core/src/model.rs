//! The catalogue and event maps a room document holds.
//!
//! Both roots are maps keyed by uuid string. An event keeps its times as text,
//! its tags and products as maps used as sets, and its rating as a number or
//! null; a catalogue keeps a map of the uuids it lists. Reading this layout is
//! pure: every function borrows a read transaction and returns owned values.
//! Writing it is the same layout in reverse, in the caller's transaction: what
//! Python cocat's client writes, so either side reads the other's documents.

use std::sync::Arc;

use serde::Serialize;
use serde_json::{Map, Value};
use thiserror::Error;
use tracing::warn;
use yrs::types::ToJson as _;
use yrs::{Any, In, Map as _, MapPrelim, MapRef, Number, Out, ReadTxn, TransactionMut, WriteTxn};

use crate::time::Timestamp;

pub const CATALOGUES: &str = "catalogues";
pub const EVENTS: &str = "events";

/// The fields a catalogue is created from. The uuid is the caller's: Python only
/// generates one when its user asks for a default.
pub struct NewCatalogue<'a> {
    pub uuid: &'a str,
    pub name: &'a str,
    pub author: &'a str,
    pub tags: &'a [String],
    pub attributes: &'a Map<String, Value>,
}

pub struct NewEvent<'a> {
    pub uuid: &'a str,
    pub start: Timestamp,
    pub stop: Timestamp,
    pub author: &'a str,
    pub tags: &'a [String],
    pub products: &'a [String],
    pub rating: Option<i64>,
    pub attributes: &'a Map<String, Value>,
}

#[derive(Debug, Error)]
pub enum WriteError {
    #[error("attribute {key} is not a value a room document holds")]
    Attribute {
        key: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("time {0} cannot be written")]
    Time(#[from] time::error::Format),
}

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

/// One event by uuid, or `None` when the room does not hold it.
pub fn event<T: ReadTxn>(txn: &T, uuid: &str) -> Option<EventRecord> {
    let events = txn.get_map(EVENTS)?;
    let Out::YMap(map) = events.get(txn, uuid)? else {
        return None;
    };
    record(txn, &map)
}

/// The uuids of the catalogues that list `uuid`, sorted.
pub fn event_catalogues<T: ReadTxn>(txn: &T, uuid: &str) -> Vec<String> {
    let Some(root) = txn.get_map(CATALOGUES) else {
        return Vec::new();
    };
    let mut catalogues: Vec<String> = root
        .iter(txn)
        .filter_map(|(key, value)| match value {
            Out::YMap(map)
                if membership(&map, txn, "events")
                    .iter()
                    .any(|event| event == uuid) =>
            {
                Some(key.to_owned())
            }
            _ => None,
        })
        .collect();
    catalogues.sort_unstable();
    catalogues
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

pub fn create_catalogue(
    txn: &mut TransactionMut<'_>,
    catalogue: &NewCatalogue<'_>,
) -> Result<(), WriteError> {
    let mut content = MapPrelim::default();
    content.insert(Arc::from("uuid"), In::from(catalogue.uuid));
    content.insert(Arc::from("name"), In::from(catalogue.name));
    content.insert(Arc::from("author"), In::from(catalogue.author));
    content.insert(Arc::from("tags"), set_prelim(catalogue.tags));
    content.insert(Arc::from("events"), set_prelim(&[]));
    content.insert(
        Arc::from("attributes"),
        attributes_prelim(catalogue.attributes)?,
    );
    txn.get_or_insert_map(CATALOGUES)
        .insert(txn, catalogue.uuid, content);
    Ok(())
}

pub fn create_event(txn: &mut TransactionMut<'_>, event: &NewEvent<'_>) -> Result<(), WriteError> {
    let mut content = MapPrelim::default();
    content.insert(Arc::from("uuid"), In::from(event.uuid));
    content.insert(Arc::from("start"), In::from(event.start.as_text()?));
    content.insert(Arc::from("stop"), In::from(event.stop.as_text()?));
    content.insert(Arc::from("author"), In::from(event.author));
    content.insert(Arc::from("tags"), set_prelim(event.tags));
    content.insert(Arc::from("products"), set_prelim(event.products));
    content.insert(Arc::from("rating"), rating_prelim(event.rating));
    content.insert(
        Arc::from("attributes"),
        attributes_prelim(event.attributes)?,
    );
    txn.get_or_insert_map(EVENTS)
        .insert(txn, event.uuid, content);
    Ok(())
}

pub fn rename_catalogue(txn: &mut TransactionMut<'_>, uuid: &str, name: &str) -> bool {
    let Some(catalogue) = object(txn, CATALOGUES, uuid) else {
        return false;
    };
    catalogue.insert(txn, "name", name);
    true
}

pub fn set_catalogue_attributes(
    txn: &mut TransactionMut<'_>,
    uuid: &str,
    attributes: &Map<String, Value>,
) -> Result<bool, WriteError> {
    set_attributes(txn, CATALOGUES, uuid, attributes)
}

pub fn set_event_attributes(
    txn: &mut TransactionMut<'_>,
    uuid: &str,
    attributes: &Map<String, Value>,
) -> Result<bool, WriteError> {
    set_attributes(txn, EVENTS, uuid, attributes)
}

pub fn remove_catalogue_attributes(
    txn: &mut TransactionMut<'_>,
    uuid: &str,
    keys: &[String],
) -> bool {
    remove_keys(txn, CATALOGUES, uuid, "attributes", keys)
}

pub fn remove_event_attributes(txn: &mut TransactionMut<'_>, uuid: &str, keys: &[String]) -> bool {
    remove_keys(txn, EVENTS, uuid, "attributes", keys)
}

pub fn add_catalogue_events(txn: &mut TransactionMut<'_>, uuid: &str, events: &[String]) -> bool {
    let Some(catalogue) = object(txn, CATALOGUES, uuid) else {
        return false;
    };
    let listed: MapRef = catalogue.get_or_init(txn, "events");
    for event in events {
        listed.insert(txn, event.as_str(), true);
    }
    true
}

pub fn remove_catalogue_events(
    txn: &mut TransactionMut<'_>,
    uuid: &str,
    events: &[String],
) -> bool {
    remove_keys(txn, CATALOGUES, uuid, "events", events)
}

pub fn set_event_range(
    txn: &mut TransactionMut<'_>,
    uuid: &str,
    start: Timestamp,
    stop: Timestamp,
) -> Result<bool, WriteError> {
    let Some(event) = object(txn, EVENTS, uuid) else {
        return Ok(false);
    };
    let start = start.as_text()?;
    let stop = stop.as_text()?;
    event.insert(txn, "start", start);
    event.insert(txn, "stop", stop);
    Ok(true)
}

/// Removes the event, as Python does, from the room and from every catalogue listing it.
pub fn delete_event(txn: &mut TransactionMut<'_>, uuid: &str) -> bool {
    let Some(events) = txn.get_map(EVENTS) else {
        return false;
    };
    if events.remove(txn, uuid).is_none() {
        return false;
    }
    for catalogue in children(txn, CATALOGUES) {
        if let Some(listed) = nested(&catalogue, txn, "events") {
            listed.remove(txn, uuid);
        }
    }
    true
}

pub fn delete_catalogue(txn: &mut TransactionMut<'_>, uuid: &str) -> bool {
    let Some(catalogues) = txn.get_map(CATALOGUES) else {
        return false;
    };
    catalogues.remove(txn, uuid).is_some()
}

fn set_attributes(
    txn: &mut TransactionMut<'_>,
    root: &str,
    uuid: &str,
    attributes: &Map<String, Value>,
) -> Result<bool, WriteError> {
    let Some(object) = object(txn, root, uuid) else {
        return Ok(false);
    };
    let values = attributes
        .iter()
        .map(|(key, value)| Ok((key.as_str(), any_value(key, value)?)))
        .collect::<Result<Vec<_>, WriteError>>()?;
    let target: MapRef = object.get_or_init(txn, "attributes");
    for (key, value) in values {
        target.insert(txn, key, value);
    }
    Ok(true)
}

fn remove_keys(
    txn: &mut TransactionMut<'_>,
    root: &str,
    uuid: &str,
    field: &str,
    keys: &[String],
) -> bool {
    let Some(object) = object(txn, root, uuid) else {
        return false;
    };
    let Some(entries) = nested(&object, txn, field) else {
        return true;
    };
    for key in keys {
        entries.remove(txn, key);
    }
    true
}

fn set_prelim(entries: &[String]) -> In {
    let mut set = MapPrelim::default();
    for entry in entries {
        set.insert(Arc::from(entry.as_str()), In::from(true));
    }
    In::from(set)
}

fn attributes_prelim(attributes: &Map<String, Value>) -> Result<In, WriteError> {
    let mut prelim = MapPrelim::default();
    for (key, value) in attributes {
        prelim.insert(Arc::from(key.as_str()), In::from(any_value(key, value)?));
    }
    Ok(In::from(prelim))
}

fn rating_prelim(rating: Option<i64>) -> In {
    match rating {
        Some(rating) => In::from(rating),
        None => In::from(Any::Null),
    }
}

fn any_value(key: &str, value: &Value) -> Result<Any, WriteError> {
    serde_json::from_value(value.clone()).map_err(|source| WriteError::Attribute {
        key: key.to_owned(),
        source,
    })
}

fn object<T: ReadTxn>(txn: &T, root: &str, uuid: &str) -> Option<MapRef> {
    match txn.get_map(root)?.get(txn, uuid)? {
        Out::YMap(map) => Some(map),
        _ => None,
    }
}

fn nested<T: ReadTxn>(object: &MapRef, txn: &T, field: &str) -> Option<MapRef> {
    match object.get(txn, field)? {
        Out::YMap(map) => Some(map),
        _ => None,
    }
}

fn children<T: ReadTxn>(txn: &T, root: &str) -> Vec<MapRef> {
    txn.get_map(root)
        .map(|root| {
            root.iter(txn)
                .filter_map(|(_, value)| match value {
                    Out::YMap(map) => Some(map),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}
