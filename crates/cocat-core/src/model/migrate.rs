//! The one place that still reads the first layout: Python cocat's, with times as text,
//! sets as maps, a 0–10 rating and nested attribute maps. A room is migrated once, when
//! the server imports it.

use serde_json::{Map, Value};
use yrs::types::ToJson as _;
use yrs::{Doc, Map as _, MapRef, Out, ReadTxn, Transact};

use super::read::{entries, is_v1, members};
use super::values::{number, text};
use super::write::{add_catalogue_events, create_catalogue, create_event, mark_layout};
use super::{CATALOGUES, EVENTS, EventRecord, NewCatalogue, WriteError};
use crate::time::Timestamp;

/// Rewrites a v1 document in the v2 layout, in place, and returns the update that does it
/// so it can be stored; `None` when the document is not v1. An event whose times are not
/// times is left as it was: no v2 reader can read it either way.
pub fn migrate_v1(doc: &Doc) -> Result<Option<Vec<u8>>, WriteError> {
    let mut txn = doc.transact_mut();
    if !is_v1(&txn) {
        return Ok(None);
    }
    let events: Vec<EventRecord> = entries(&txn, EVENTS)
        .iter()
        .filter_map(|(uuid, map)| event(&txn, uuid, map))
        .collect();
    let catalogues: Vec<(NewCatalogue, Vec<String>)> = entries(&txn, CATALOGUES)
        .iter()
        .map(|(uuid, map)| catalogue(&txn, uuid, map))
        .collect();
    for event in &events {
        create_event(&mut txn, event)?;
    }
    for (catalogue, listed) in &catalogues {
        create_catalogue(&mut txn, catalogue)?;
        add_catalogue_events(&mut txn, &catalogue.uuid, listed);
    }
    mark_layout(&mut txn);
    Ok(Some(txn.encode_update_v1()))
}

fn event<T: ReadTxn>(txn: &T, uuid: &str, map: &MapRef) -> Option<EventRecord> {
    let start = Timestamp::parse(&field(txn, map, "start")?)?;
    let stop = Timestamp::parse(&field(txn, map, "stop")?)?;
    let mut record = EventRecord::new(uuid, start, stop);
    record.author = field(txn, map, "author").filter(|author| !author.is_empty());
    record.tags = set(txn, map, "tags");
    record.products = set(txn, map, "products");
    record.confidence = rating(txn, map).map(|rating| (rating / 10.0).clamp(0.0, 1.0));
    record.attributes = attributes(txn, map);
    Some(record)
}

fn catalogue<T: ReadTxn>(txn: &T, uuid: &str, map: &MapRef) -> (NewCatalogue, Vec<String>) {
    let catalogue = NewCatalogue {
        uuid: uuid.to_owned(),
        name: field(txn, map, "name").unwrap_or_default(),
        author: field(txn, map, "author").filter(|author| !author.is_empty()),
        tags: set(txn, map, "tags"),
        attributes: attributes(txn, map),
        ..NewCatalogue::default()
    };
    (catalogue, members(txn, map))
}

fn field<T: ReadTxn>(txn: &T, map: &MapRef, key: &str) -> Option<String> {
    match map.get(txn, key)? {
        Out::Any(any) => text(&any),
        _ => None,
    }
}

fn rating<T: ReadTxn>(txn: &T, map: &MapRef) -> Option<f64> {
    match map.get(txn, "rating")? {
        Out::Any(any) => number(&any),
        _ => None,
    }
}

fn set<T: ReadTxn>(txn: &T, map: &MapRef, key: &str) -> Vec<String> {
    match map.get(txn, key) {
        Some(Out::YMap(set)) => set.keys(txn).map(str::to_owned).collect(),
        _ => Vec::new(),
    }
}

fn attributes<T: ReadTxn>(txn: &T, map: &MapRef) -> Map<String, Value> {
    match map.get(txn, "attributes") {
        Some(Out::YMap(attributes)) => match serde_json::to_value(attributes.to_json(txn)) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        },
        _ => Map::new(),
    }
}
