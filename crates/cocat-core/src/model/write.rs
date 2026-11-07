//! Writing events and catalogues in the caller's transaction. A function that names an
//! object the room does not hold changes nothing and says so (`false` or `None`).

use std::sync::Arc;

use serde_json::{Map, Value};
use yrs::{Any, In, Map as _, MapPrelim, MapRef, ReadTxn as _, TransactionMut, WriteTxn};

use super::read::{entries, object};
use super::registry;
use super::values::{json_any, shape_any, strings_any, time_any};
use super::{
    ATTRIBUTE_PREFIX, CATALOGUES, COMMENTS, EVENTS, EventField, EventRecord, LAYOUT, META,
    NewCatalogue, Status, TypeWarning, Vertex, WriteError,
};
use crate::rules;
use crate::time::Timestamp;

pub fn create_event(
    txn: &mut TransactionMut<'_>,
    record: &EventRecord,
) -> Result<Vec<TypeWarning>, WriteError> {
    let (start, stop) = match &record.shape {
        Some(shape) => span(shape)?,
        None => (record.start, record.stop),
    };
    let mut content = MapPrelim::default();
    content.insert(Arc::from("start"), In::Any(time_any(start)?));
    content.insert(Arc::from("stop"), In::Any(time_any(stop)?));
    for field in fields(record) {
        if let (key, Some(value)) = entry(&field)? {
            content.insert(Arc::from(key), In::Any(value));
        }
    }
    for (key, value) in attribute_entries(&record.attributes)? {
        content.insert(Arc::from(key), In::Any(value));
    }
    let warnings = registry::check(txn, &record.attributes);
    txn.get_or_insert_map(EVENTS)
        .insert(txn, record.uuid.as_str(), content);
    mark_layout(txn);
    Ok(warnings)
}

pub fn create_catalogue(
    txn: &mut TransactionMut<'_>,
    catalogue: &NewCatalogue,
) -> Result<(), WriteError> {
    let mut content = MapPrelim::default();
    content.insert(Arc::from("name"), In::from(catalogue.name.as_str()));
    content.insert(Arc::from("events"), In::from(MapPrelim::default()));
    if let Some(author) = &catalogue.author {
        content.insert(Arc::from("author"), In::from(author.as_str()));
    }
    if let Some(created) = catalogue.created {
        content.insert(Arc::from("created"), In::Any(time_any(created)?));
    }
    if let Some(tags) = strings_any(&catalogue.tags) {
        content.insert(Arc::from("tags"), In::Any(tags));
    }
    if let Some(columns) = list_any(&catalogue.columns) {
        content.insert(Arc::from("columns"), In::Any(columns));
    }
    for (key, value) in attribute_entries(&catalogue.attributes)? {
        content.insert(Arc::from(key), In::Any(value));
    }
    txn.get_or_insert_map(CATALOGUES)
        .insert(txn, catalogue.uuid.as_str(), content);
    mark_layout(txn);
    Ok(())
}

pub fn rename_catalogue(txn: &mut TransactionMut<'_>, uuid: &str, name: &str) -> bool {
    set(txn, CATALOGUES, uuid, &[("name", Some(Any::from(name)))])
}

/// The attribute keys a catalogue shows as columns, in the order given.
pub fn set_catalogue_columns(txn: &mut TransactionMut<'_>, uuid: &str, columns: &[String]) -> bool {
    set(txn, CATALOGUES, uuid, &[("columns", list_any(columns))])
}

pub fn set_catalogue_tags(txn: &mut TransactionMut<'_>, uuid: &str, tags: &[String]) -> bool {
    set(txn, CATALOGUES, uuid, &[("tags", strings_any(tags))])
}

pub fn set_catalogue_rule(
    txn: &mut TransactionMut<'_>,
    uuid: &str,
    rule: Option<&Value>,
) -> Result<bool, WriteError> {
    if let Some(rule) = rule {
        rules::parse(rule)?;
    }
    let rule = rule.map(|rule| json_any("rule", rule)).transpose()?;
    Ok(set(txn, CATALOGUES, uuid, &[("rule", rule)]))
}

pub fn set_catalogue_attributes(
    txn: &mut TransactionMut<'_>,
    uuid: &str,
    attributes: &Map<String, Value>,
) -> Result<bool, WriteError> {
    let entries = attribute_entries(attributes)?;
    Ok(set_attributes(txn, CATALOGUES, uuid, entries))
}

/// Merges the attributes into the event's; `None` when the room holds no such event.
pub fn set_event_attributes(
    txn: &mut TransactionMut<'_>,
    uuid: &str,
    attributes: &Map<String, Value>,
) -> Result<Option<Vec<TypeWarning>>, WriteError> {
    let entries = attribute_entries(attributes)?;
    if !set_attributes(txn, EVENTS, uuid, entries) {
        return Ok(None);
    }
    Ok(Some(registry::check(txn, attributes)))
}

pub fn remove_catalogue_attributes(
    txn: &mut TransactionMut<'_>,
    uuid: &str,
    keys: &[String],
) -> bool {
    set(txn, CATALOGUES, uuid, &removals(keys))
}

pub fn remove_event_attributes(txn: &mut TransactionMut<'_>, uuid: &str, keys: &[String]) -> bool {
    set(txn, EVENTS, uuid, &removals(keys))
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
    let Some(catalogue) = object(txn, CATALOGUES, uuid) else {
        return false;
    };
    if let Some(yrs::Out::YMap(listed)) = catalogue.get(txn, "events") {
        for event in events {
            listed.remove(txn, event);
        }
    }
    true
}

pub fn set_event_range(
    txn: &mut TransactionMut<'_>,
    uuid: &str,
    start: Timestamp,
    stop: Timestamp,
) -> Result<bool, WriteError> {
    let range = [
        ("start", Some(time_any(start)?)),
        ("stop", Some(time_any(stop)?)),
    ];
    Ok(set(txn, EVENTS, uuid, &range))
}

pub fn set_event_field(
    txn: &mut TransactionMut<'_>,
    uuid: &str,
    field: &EventField,
) -> Result<bool, WriteError> {
    let mut entries = vec![entry(field)?];
    if let EventField::Shape(Some(shape)) = field {
        let (start, stop) = span(shape)?;
        entries.push(("start", Some(time_any(start)?)));
        entries.push(("stop", Some(time_any(stop)?)));
    }
    Ok(set(txn, EVENTS, uuid, &entries))
}

/// Moves the event to the trash: every list leaves it out until it is restored.
pub fn delete_event(txn: &mut TransactionMut<'_>, uuid: &str) -> bool {
    set(txn, EVENTS, uuid, &[status(Status::Deleted)])
}

pub fn restore_event(txn: &mut TransactionMut<'_>, uuid: &str) -> bool {
    set(txn, EVENTS, uuid, &[status(Status::Confirmed)])
}

/// Removes the event for good: from the room, from every catalogue and its comment.
pub fn purge_event(txn: &mut TransactionMut<'_>, uuid: &str) -> bool {
    let Some(events) = txn.get_map(EVENTS) else {
        return false;
    };
    if events.remove(txn, uuid).is_none() {
        return false;
    }
    for (_, catalogue) in entries(txn, CATALOGUES) {
        if let Some(yrs::Out::YMap(listed)) = catalogue.get(txn, "events") {
            listed.remove(txn, uuid);
        }
    }
    if let Some(comments) = txn.get_map(COMMENTS) {
        comments.remove(txn, uuid);
    }
    true
}

pub fn delete_catalogue(txn: &mut TransactionMut<'_>, uuid: &str) -> bool {
    set(txn, CATALOGUES, uuid, &[status(Status::Deleted)])
}

pub fn restore_catalogue(txn: &mut TransactionMut<'_>, uuid: &str) -> bool {
    set(txn, CATALOGUES, uuid, &[status(Status::Confirmed)])
}

pub fn purge_catalogue(txn: &mut TransactionMut<'_>, uuid: &str) -> bool {
    let Some(catalogues) = txn.get_map(CATALOGUES) else {
        return false;
    };
    catalogues.remove(txn, uuid).is_some()
}

/// The layout marker goes in with the first content, so a reader can tell this room from a
/// v1 one.
pub(super) fn mark_layout(txn: &mut TransactionMut<'_>) {
    let meta = txn.get_or_insert_map(META);
    if !meta.contains_key(txn, "layout") {
        meta.insert(txn, "layout", LAYOUT);
    }
}

/// Writes or removes (`None`) each key of one object. Returns false when there is no such
/// object.
fn set<K: AsRef<str>>(
    txn: &mut TransactionMut<'_>,
    root: &str,
    uuid: &str,
    entries: &[(K, Option<Any>)],
) -> bool {
    let Some(object) = object(txn, root, uuid) else {
        return false;
    };
    for (key, value) in entries {
        match value {
            Some(value) => {
                object.insert(txn, key.as_ref(), value.clone());
            }
            None => {
                object.remove(txn, key.as_ref());
            }
        }
    }
    true
}

fn set_attributes(
    txn: &mut TransactionMut<'_>,
    root: &str,
    uuid: &str,
    attributes: Vec<(String, Any)>,
) -> bool {
    let entries: Vec<(String, Option<Any>)> = attributes
        .into_iter()
        .map(|(key, value)| (key, Some(value)))
        .collect();
    set(txn, root, uuid, &entries)
}

fn attribute_key(key: &str) -> String {
    format!("{ATTRIBUTE_PREFIX}{key}")
}

fn attribute_entries(attributes: &Map<String, Value>) -> Result<Vec<(String, Any)>, WriteError> {
    attributes
        .iter()
        .map(|(key, value)| Ok((attribute_key(key), json_any(key, value)?)))
        .collect()
}

fn removals(keys: &[String]) -> Vec<(String, Option<Any>)> {
    keys.iter().map(|key| (attribute_key(key), None)).collect()
}

fn status(status: Status) -> (&'static str, Option<Any>) {
    match status {
        Status::Confirmed => ("status", None),
        other => ("status", Some(Any::from(other.name()))),
    }
}

/// The fields of a record other than its range and attributes, as written on creation.
fn fields(record: &EventRecord) -> [EventField; 11] {
    [
        EventField::Author(record.author.clone()),
        EventField::Created(record.created),
        EventField::Origin(record.origin.clone()),
        EventField::Status(record.status),
        EventField::Confidence(record.confidence),
        EventField::StartErr(record.start_err),
        EventField::StopErr(record.stop_err),
        EventField::Shape(record.shape.clone()),
        EventField::SourceId(record.source_id.clone()),
        EventField::Tags(record.tags.clone()),
        EventField::Products(record.products.clone()),
    ]
}

/// The key a field is stored under and its value, or `None` when the field holds its
/// default and is therefore not stored.
fn entry(field: &EventField) -> Result<(&'static str, Option<Any>), WriteError> {
    let text = |value: &Option<String>| value.as_deref().map(Any::from);
    let number = |value: &Option<f64>| value.map(Any::from);
    Ok(match field {
        EventField::Author(value) => ("author", text(value)),
        EventField::Created(value) => ("created", value.map(time_any).transpose()?),
        EventField::Origin(value) => ("origin", text(value)),
        EventField::Status(value) => status(*value),
        EventField::Confidence(value) => ("confidence", number(&confidence(*value)?)),
        EventField::StartErr(value) => ("start_err", number(value)),
        EventField::StopErr(value) => ("stop_err", number(value)),
        EventField::Shape(value) => ("shape", value.as_deref().map(shape_any).transpose()?),
        EventField::SourceId(value) => ("source_id", text(value)),
        EventField::Tags(value) => ("tags", strings_any(value)),
        EventField::Products(value) => ("products", strings_any(value)),
    })
}

fn confidence(value: Option<f64>) -> Result<Option<f64>, WriteError> {
    match value {
        Some(value) if !(0.0..=1.0).contains(&value) => Err(WriteError::Confidence(value)),
        other => Ok(other),
    }
}

/// The time span of a shape, which becomes the event's range.
fn span(shape: &[Vertex]) -> Result<(Timestamp, Timestamp), WriteError> {
    let times = shape.iter().map(|vertex| vertex.time);
    match (times.clone().min(), times.max()) {
        (Some(start), Some(stop)) => Ok((start, stop)),
        _ => Err(WriteError::EmptyShape),
    }
}

/// A list kept in the order given, or `None` for an empty one.
fn list_any(items: &[String]) -> Option<Any> {
    if items.is_empty() {
        return None;
    }
    Some(Any::Array(
        items.iter().map(|item| Any::from(item.as_str())).collect(),
    ))
}
