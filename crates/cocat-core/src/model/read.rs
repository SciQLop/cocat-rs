//! Reading events and catalogues. Deleted ones are left out of every list; looking one up
//! by uuid still finds it, so it can be restored.

use std::collections::BTreeSet;

use serde_json::{Map, Value};
use yrs::{Any, Map as _, MapRef, Out, ReadTxn};

use super::copies::chain_in;
use super::values::{get, json, number, shape, strings, text, time, typed};
use super::{
    ATTRIBUTE_PREFIX, CATALOGUES, CatalogueSummary, EVENTS, EventRecord, META, SEARCH_TEXT, Status,
};

/// The catalogues not in the trash, sorted by name then uuid.
pub fn catalogues<T: ReadTxn>(txn: &T) -> Vec<CatalogueSummary> {
    let mut catalogues: Vec<CatalogueSummary> = entries(txn, CATALOGUES)
        .into_iter()
        .map(|(uuid, map)| summary(txn, &uuid, &map))
        .filter(|catalogue| !catalogue.deleted)
        .collect();
    catalogues.sort_by(|left, right| {
        (left.name.as_str(), left.uuid.as_str()).cmp(&(right.name.as_str(), right.uuid.as_str()))
    });
    catalogues
}

/// Every catalogue the room holds, the trash included, sorted by uuid.
pub fn all_catalogues<T: ReadTxn>(txn: &T) -> Vec<CatalogueSummary> {
    let mut catalogues: Vec<CatalogueSummary> = entries(txn, CATALOGUES)
        .into_iter()
        .map(|(uuid, map)| summary(txn, &uuid, &map))
        .collect();
    catalogues.sort_by(|left, right| left.uuid.cmp(&right.uuid));
    catalogues
}

/// The catalogues not in the trash as `(uuid, name)`, in `catalogues`' order: how a search
/// names them. Only the name and status are read: a whole summary counts the events.
pub fn catalogue_names<T: ReadTxn>(txn: &T) -> Vec<(String, String)> {
    let mut names: Vec<(String, String)> = entries(txn, CATALOGUES)
        .into_iter()
        .filter(|(_, map)| !is_deleted(txn, map))
        .map(|(uuid, map)| {
            let name = get(&map, txn, "name").as_ref().and_then(text);
            (uuid, name.unwrap_or_default())
        })
        .collect();
    names.sort_by(|left, right| (&left.1, &left.0).cmp(&(&right.1, &right.0)));
    names
}

/// The text a catalogue's search was typed as, when it was set from text (see
/// `query::where_text`, which checks it still reads as the rule).
pub fn search_text<T: ReadTxn>(txn: &T, uuid: &str) -> Option<String> {
    get(&object(txn, CATALOGUES, uuid)?, txn, SEARCH_TEXT)
        .as_ref()
        .and_then(text)
}

/// One catalogue by uuid, deleted or not.
pub fn catalogue<T: ReadTxn>(txn: &T, uuid: &str) -> Option<CatalogueSummary> {
    Some(summary(txn, uuid, &object(txn, CATALOGUES, uuid)?))
}

/// One event by uuid, deleted or not, or `None` when the room does not hold a readable one.
pub fn event<T: ReadTxn>(txn: &T, uuid: &str) -> Option<EventRecord> {
    record(txn, uuid, &object(txn, EVENTS, uuid)?)
}

/// The uuids of the events a catalogue lists (its rule aside), trashed or not, sorted.
pub fn listed_events<T: ReadTxn>(txn: &T, uuid: &str) -> Vec<String> {
    let mut listed = object(txn, CATALOGUES, uuid)
        .map(|catalogue| members(txn, &catalogue))
        .unwrap_or_default();
    listed.sort_unstable();
    listed
}

/// How many events the room holds outside the trash.
/// The attribute keys a search of the room can name: those its registry types and those its
/// live events carry, sorted. A relayed list may write events without registering their keys.
pub fn attribute_keys<T: ReadTxn>(txn: &T) -> Vec<String> {
    let mut keys: BTreeSet<String> = super::attribute_types(txn).into_keys().collect();
    for (_, event) in entries(txn, EVENTS) {
        if is_deleted(txn, &event) {
            continue;
        }
        let carried = event
            .keys(txn)
            .filter_map(|key| key.strip_prefix(ATTRIBUTE_PREFIX));
        keys.extend(carried.map(str::to_owned));
    }
    keys.into_iter().collect()
}

pub fn live_event_count<T: ReadTxn>(txn: &T) -> usize {
    entries(txn, EVENTS)
        .iter()
        .filter(|(_, map)| !is_deleted(txn, map))
        .count()
}

/// The events in the trash, in time order.
pub fn trash<T: ReadTxn>(txn: &T) -> Vec<EventRecord> {
    let mut records: Vec<EventRecord> = entries(txn, EVENTS)
        .into_iter()
        .filter_map(|(uuid, map)| record(txn, &uuid, &map))
        .filter(|record| record.status == Status::Deleted)
        .collect();
    in_time_order(&mut records);
    records
}

/// The uuids of the catalogues not in the trash that list `uuid`, sorted.
pub fn event_catalogues<T: ReadTxn>(txn: &T, uuid: &str) -> Vec<String> {
    let mut catalogues: Vec<String> = entries(txn, CATALOGUES)
        .into_iter()
        .filter(|(_, map)| !is_deleted(txn, map) && members(txn, map).iter().any(|e| e == uuid))
        .map(|(key, _)| key)
        .collect();
    catalogues.sort_unstable();
    catalogues
}

/// Every tag an event or a catalogue not in the trash carries, sorted.
pub fn existing_tags<T: ReadTxn>(txn: &T) -> Vec<String> {
    let mut tags = BTreeSet::new();
    for map in [EVENTS, CATALOGUES]
        .into_iter()
        .flat_map(|root| live(txn, root))
    {
        if let Some(Any::Array(items)) = get(&map, txn, "tags") {
            items.iter().for_each(|tag| add_text(&mut tags, tag));
        }
    }
    tags.into_iter().collect()
}

/// Adds a text to a set, copying it only when it is new: a project holds many events and
/// few distinct tags and authors.
pub(super) fn add_text(set: &mut BTreeSet<String>, any: &Any) {
    if let Any::String(text) = any
        && !set.contains(text.as_ref())
    {
        set.insert(text.to_string());
    }
}

/// The objects of a root not in the trash.
pub(super) fn live<'a, T: ReadTxn>(txn: &'a T, root: &str) -> impl Iterator<Item = MapRef> + 'a {
    txn.get_map(root)
        .into_iter()
        .flat_map(move |root| root.iter(txn).map(|(_, value)| value).collect::<Vec<_>>())
        .filter_map(|value| match value {
            Out::YMap(map) => Some(map),
            _ => None,
        })
        .filter(move |map| !is_deleted(txn, map))
}

/// Whether the room holds content written in the first layout: something in it, and no
/// layout marker.
pub fn is_v1<T: ReadTxn>(txn: &T) -> bool {
    let marked = txn
        .get_map(META)
        .is_some_and(|meta| meta.contains_key(txn, "layout"));
    let filled = [EVENTS, CATALOGUES]
        .into_iter()
        .any(|root| txn.get_map(root).is_some_and(|map| map.len(txn) > 0));
    filled && !marked
}

fn in_time_order(records: &mut [EventRecord]) {
    records.sort_by(|left, right| {
        (left.start, left.uuid.as_str()).cmp(&(right.start, right.uuid.as_str()))
    });
}

fn summary<T: ReadTxn>(txn: &T, uuid: &str, map: &MapRef) -> CatalogueSummary {
    let mut summary = CatalogueSummary {
        uuid: uuid.to_owned(),
        name: String::new(),
        author: None,
        created: None,
        deleted: false,
        tags: Vec::new(),
        columns: Vec::new(),
        attributes: Map::new(),
        rule: None,
        shape_axis: None,
        copied_from: chain_in(txn, map),
        event_count: 0,
    };
    for (key, value) in map.iter(txn) {
        if key == "events" {
            summary.event_count = live_members(txn, &value);
            continue;
        }
        let Out::Any(any) = value else { continue };
        match key {
            "name" => summary.name = text(&any).unwrap_or_default(),
            "author" => summary.author = text(&any),
            "created" => summary.created = time(&any),
            "status" => summary.deleted = status(&any) == Status::Deleted,
            "tags" => summary.tags = strings(&any),
            "columns" => summary.columns = strings(&any),
            "rule" => summary.rule = json(&any),
            "shape_axis" => summary.shape_axis = typed(&any),
            _ => attribute(&mut summary.attributes, key, &any),
        }
    }
    summary
}

fn record<T: ReadTxn>(txn: &T, uuid: &str, map: &MapRef) -> Option<EventRecord> {
    let start = get(map, txn, "start").as_ref().and_then(time)?;
    let stop = get(map, txn, "stop").as_ref().and_then(time)?;
    let mut record = EventRecord::new(uuid, start, stop);
    for (key, value) in map.iter(txn) {
        let Out::Any(any) = value else { continue };
        match key {
            "start" | "stop" => {}
            "author" => record.author = text(&any),
            "created" => record.created = time(&any),
            "origin" => record.origin = text(&any),
            "status" => record.status = status(&any),
            "confidence" => record.confidence = number(&any),
            "start_err" => record.start_err = number(&any),
            "stop_err" => record.stop_err = number(&any),
            "shape" => record.shape = shape(&any),
            "source_id" => record.source_id = text(&any),
            "tags" => record.tags = strings(&any),
            "products" => record.products = strings(&any),
            _ => attribute(&mut record.attributes, key, &any),
        }
    }
    Some(record)
}

fn attribute(attributes: &mut Map<String, Value>, key: &str, any: &yrs::Any) {
    if let Some(name) = key.strip_prefix(ATTRIBUTE_PREFIX)
        && let Some(value) = json(any)
    {
        attributes.insert(name.to_owned(), value);
    }
}

pub(super) fn is_deleted<T: ReadTxn>(txn: &T, map: &MapRef) -> bool {
    matches!(map.get(txn, "status"), Some(Out::Any(any)) if status(&any) == Status::Deleted)
}

/// An unknown status reads as the default, so a newer client's status does not hide an
/// event from an older one.
pub(super) fn status(any: &yrs::Any) -> Status {
    text(any)
        .and_then(|name| Status::from_name(&name))
        .unwrap_or_default()
}

/// How many of the listed events exist and are not in the trash.
fn live_members<T: ReadTxn>(txn: &T, listed: &Out) -> usize {
    let Out::YMap(listed) = listed else { return 0 };
    listed
        .keys(txn)
        .filter(|event| object(txn, EVENTS, event).is_some_and(|map| !is_deleted(txn, &map)))
        .count()
}

pub(super) fn members<T: ReadTxn>(txn: &T, catalogue: &MapRef) -> Vec<String> {
    match catalogue.get(txn, "events") {
        Some(Out::YMap(listed)) => listed.keys(txn).map(str::to_owned).collect(),
        _ => Vec::new(),
    }
}

pub(super) fn object<T: ReadTxn>(txn: &T, root: &str, uuid: &str) -> Option<MapRef> {
    match txn.get_map(root)?.get(txn, uuid)? {
        Out::YMap(map) => Some(map),
        _ => None,
    }
}

pub(super) fn entries<T: ReadTxn>(txn: &T, root: &str) -> Vec<(String, MapRef)> {
    let Some(root) = txn.get_map(root) else {
        return Vec::new();
    };
    root.iter(txn)
        .filter_map(|(key, value)| match value {
            Out::YMap(map) => Some((key.to_owned(), map)),
            _ => None,
        })
        .collect()
}
