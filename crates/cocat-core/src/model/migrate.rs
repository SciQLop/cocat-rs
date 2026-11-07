//! The one place that still reads the first layout: Python cocat's, with times as text,
//! sets as maps, a 0–10 rating and nested attribute maps. A room is migrated once, when
//! the server imports it.

use std::collections::HashSet;

use serde_json::{Map, Value};
use yrs::types::ToJson as _;
use yrs::{Doc, Map as _, MapRef, Out, ReadTxn, Transact};

use super::read::{entries, is_v1, members};
use super::values::{number, text};
use super::write::{add_catalogue_events, create_catalogue, create_event, mark_layout};
use super::{CATALOGUES, EVENTS, EventRecord, NewCatalogue, WriteError};
use crate::time::Timestamp;

/// What migrating one room did: the update to store, and the events dropped because their
/// times do not read as times (no layout can show or edit those).
#[derive(Debug)]
pub struct Migration {
    pub update: Vec<u8>,
    pub dropped: Vec<String>,
}

/// Rewrites a v1 document in the v2 layout, in place; `None` when the document is not v1.
/// Catalogues keep only the entries that list a migrated event.
pub fn migrate_v1(doc: &Doc) -> Result<Option<Migration>, WriteError> {
    let mut txn = doc.transact_mut();
    if !is_v1(&txn) {
        return Ok(None);
    }
    let (events, dropped): (Vec<_>, Vec<_>) = entries(&txn, EVENTS)
        .iter()
        .map(|(uuid, map)| event(&txn, uuid, map).ok_or_else(|| uuid.clone()))
        .partition(Result::is_ok);
    let events: Vec<EventRecord> = events.into_iter().flatten().collect();
    let dropped: Vec<String> = dropped.into_iter().filter_map(Result::err).collect();
    let kept: HashSet<&str> = events.iter().map(|event| event.uuid.as_str()).collect();
    let catalogues: Vec<(NewCatalogue, Vec<String>)> = entries(&txn, CATALOGUES)
        .iter()
        .map(|(uuid, map)| catalogue(&txn, uuid, map, &kept))
        .collect();
    for event in &events {
        create_event(&mut txn, event)?;
    }
    if let Some(root) = txn.get_map(EVENTS) {
        for uuid in &dropped {
            root.remove(&mut txn, uuid);
        }
    }
    for (catalogue, listed) in &catalogues {
        create_catalogue(&mut txn, catalogue)?;
        add_catalogue_events(&mut txn, &catalogue.uuid, listed);
    }
    mark_layout(&mut txn);
    Ok(Some(Migration {
        update: txn.encode_update_v1(),
        dropped,
    }))
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

fn catalogue<T: ReadTxn>(
    txn: &T,
    uuid: &str,
    map: &MapRef,
    kept: &HashSet<&str>,
) -> (NewCatalogue, Vec<String>) {
    let catalogue = NewCatalogue {
        uuid: uuid.to_owned(),
        name: field(txn, map, "name").unwrap_or_default(),
        author: field(txn, map, "author").filter(|author| !author.is_empty()),
        tags: set(txn, map, "tags"),
        attributes: attributes(txn, map),
        ..NewCatalogue::default()
    };
    let listed = members(txn, map)
        .into_iter()
        .filter(|event| kept.contains(event.as_str()))
        .collect();
    (catalogue, listed)
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
