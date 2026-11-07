//! Typed values to and from the `Any` a document holds.

use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use yrs::{Any, MapRef, Number, Out, ReadTxn};

use super::{Vertex, WriteError};
use crate::time::Timestamp;

pub(super) fn text(any: &Any) -> Option<String> {
    match any {
        Any::String(text) => Some(text.to_string()),
        _ => None,
    }
}

pub(super) fn number(any: &Any) -> Option<f64> {
    match any {
        Any::Number(Number::Float(value)) => Some(*value),
        Any::Number(Number::Int(value)) => Some(*value as f64),
        _ => None,
    }
}

/// An integer, which yrs may have encoded as a float when the float holds it exactly.
fn integer(any: &Any) -> Option<i64> {
    match any {
        Any::Number(Number::Int(value)) => Some(*value),
        Any::Number(Number::Float(value)) if value.fract() == 0.0 => Some(*value as i64),
        _ => None,
    }
}

pub(super) fn time(any: &Any) -> Option<Timestamp> {
    Timestamp::from_unix_nanos(integer(any)?)
}

pub(super) fn strings(any: &Any) -> Vec<String> {
    match any {
        Any::Array(items) => items.iter().filter_map(text).collect(),
        _ => Vec::new(),
    }
}

pub(super) fn json(any: &Any) -> Option<Value> {
    serde_json::to_value(any).ok()
}

pub(super) fn typed<T: DeserializeOwned>(any: &Any) -> Option<T> {
    serde_json::from_value(json(any)?).ok()
}

pub(super) fn shape(any: &Any) -> Option<Vec<Vertex>> {
    let Any::Array(vertices) = any else {
        return None;
    };
    vertices
        .iter()
        .map(|vertex| match vertex {
            Any::Array(pair) if pair.len() == 2 => Some(Vertex {
                time: time(&pair[0])?,
                frequency: number(&pair[1])?,
            }),
            _ => None,
        })
        .collect()
}

pub(super) fn time_any(time: Timestamp) -> Result<Any, WriteError> {
    time.unix_nanos()
        .map(Any::from)
        .ok_or(WriteError::Time(time))
}

/// A sorted list without duplicates, or `None` for an empty one: a room stores only what
/// is set.
pub(super) fn strings_any(items: &[String]) -> Option<Any> {
    let mut items: Vec<&str> = items.iter().map(String::as_str).collect();
    items.sort_unstable();
    items.dedup();
    if items.is_empty() {
        return None;
    }
    Some(Any::Array(items.into_iter().map(Any::from).collect()))
}

pub(super) fn shape_any(shape: &[Vertex]) -> Result<Any, WriteError> {
    let vertices = shape
        .iter()
        .map(|vertex| {
            Ok(Any::Array(Arc::from([
                time_any(vertex.time)?,
                Any::from(vertex.frequency),
            ])))
        })
        .collect::<Result<Vec<Any>, WriteError>>()?;
    Ok(Any::Array(Arc::from(vertices)))
}

pub(super) fn json_any(key: &str, value: &Value) -> Result<Any, WriteError> {
    serde_json::from_value(value.clone()).map_err(|source| WriteError::Value {
        key: key.to_owned(),
        source,
    })
}

pub(super) fn typed_any<T: Serialize>(key: &str, value: &T) -> Result<Any, WriteError> {
    let value = serde_json::to_value(value).map_err(|source| WriteError::Value {
        key: key.to_owned(),
        source,
    })?;
    json_any(key, &value)
}

/// The `Any` stored under `key`, when it is one.
pub(super) fn get<T: ReadTxn>(map: &MapRef, txn: &T, key: &str) -> Option<Any> {
    match yrs::Map::get(map, txn, key)? {
        Out::Any(any) => Some(any),
        _ => None,
    }
}
