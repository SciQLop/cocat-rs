//! The attribute registry: one type, and optionally a unit and a description, per event
//! attribute key in a room. A key registers itself the first time it is written, with the
//! type its value suggests; the registry warns about a mismatch and never refuses a value.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Map, Value};
use yrs::{In, Map as _, MapPrelim, MapRef, Out, ReadTxn, TransactionMut, WriteTxn};

use super::values::text;
use super::{AttributeInfo, AttributeType, REGISTRY, TypeWarning};
use crate::time::Timestamp;

pub fn attribute_types<T: ReadTxn>(txn: &T) -> BTreeMap<String, AttributeInfo> {
    let Some(registry) = txn.get_map(REGISTRY) else {
        return BTreeMap::new();
    };
    registry
        .iter(txn)
        .filter_map(|(key, value)| match value {
            Out::YMap(entry) => Some((key.to_owned(), info(txn, &entry)?)),
            _ => None,
        })
        .collect()
}

/// Registers `key` as `kind` unless it already has a type.
pub fn register_attribute(txn: &mut TransactionMut<'_>, key: &str, kind: AttributeType) {
    let registry = txn.get_or_insert_map(REGISTRY);
    if registry.contains_key(txn, key) {
        return;
    }
    let mut entry = MapPrelim::default();
    entry.insert(Arc::from("type"), In::from(kind.name()));
    registry.insert(txn, key, entry);
}

/// Sets the unit and the description of a registered key. `None` leaves a field as it is
/// and an empty text removes it. Returns false when the key is not registered.
pub fn set_attribute_info(
    txn: &mut TransactionMut<'_>,
    key: &str,
    unit: Option<&str>,
    description: Option<&str>,
) -> bool {
    let Some(Out::YMap(entry)) = txn.get_map(REGISTRY).and_then(|r| r.get(txn, key)) else {
        return false;
    };
    for (field, value) in [("unit", unit), ("description", description)] {
        match value {
            Some("") => {
                entry.remove(txn, field);
            }
            Some(value) => {
                entry.insert(txn, field, value);
            }
            None => {}
        }
    }
    true
}

/// Registers the keys not seen before and says which values do not match their type.
pub(super) fn check(
    txn: &mut TransactionMut<'_>,
    attributes: &Map<String, Value>,
) -> Vec<TypeWarning> {
    let mut warnings = Vec::new();
    for (key, value) in attributes {
        let Some(written) = guess(value) else {
            continue;
        };
        match registered(txn, key) {
            None => register_attribute(txn, key, written),
            Some(kind) if matches(kind, value) => {}
            Some(kind) => warnings.push(TypeWarning {
                key: key.clone(),
                registered: kind,
                written: value.clone(),
            }),
        }
    }
    warnings
}

/// The type a value suggests. Lists, objects and null suggest none: the registry types
/// scalar columns only.
fn guess(value: &Value) -> Option<AttributeType> {
    match value {
        Value::Number(_) => Some(AttributeType::Number),
        Value::String(_) => Some(AttributeType::Text),
        Value::Bool(_) => Some(AttributeType::Bool),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

/// A date is stored as ISO text, so text that reads as a time matches it.
fn matches(kind: AttributeType, value: &Value) -> bool {
    match (kind, value) {
        (AttributeType::Date, Value::String(text)) => Timestamp::parse(text).is_some(),
        _ => guess(value).is_none_or(|guessed| guessed == kind),
    }
}

fn registered<T: ReadTxn>(txn: &T, key: &str) -> Option<AttributeType> {
    match txn.get_map(REGISTRY)?.get(txn, key)? {
        Out::YMap(entry) => info(txn, &entry).map(|info| info.kind),
        _ => None,
    }
}

fn info<T: ReadTxn>(txn: &T, entry: &MapRef) -> Option<AttributeInfo> {
    let field = |name: &str| match entry.get(txn, name) {
        Some(Out::Any(any)) => Some(any),
        _ => None,
    };
    Some(AttributeInfo {
        kind: AttributeType::from_name(&text(&field("type")?)?)?,
        unit: field("unit").as_ref().and_then(text),
        description: field("description").as_ref().and_then(text),
    })
}
