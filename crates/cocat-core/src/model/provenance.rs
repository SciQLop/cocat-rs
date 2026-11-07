//! Origins (what made a set of events) and comments (long text kept out of the events).

use std::sync::Arc;

use yrs::{Any, In, Map as _, MapPrelim, MapRef, Out, ReadTxn, TransactionMut, WriteTxn};

use super::read::object;
use super::values::{json, json_any, text, time, time_any, typed, typed_any};
use super::write::mark_layout;
use super::{COMMENTS, EVENTS, ORIGINS, Origin, WriteError};

pub fn create_origin(txn: &mut TransactionMut<'_>, origin: &Origin) -> Result<(), WriteError> {
    let mut entries: Vec<(&str, Any)> = vec![
        ("kind", typed_any("kind", &origin.kind)?),
        ("name", Any::from(origin.name.as_str())),
    ];
    let texts = [
        ("version", &origin.version),
        ("author", &origin.author),
        ("derived_from", &origin.derived_from),
    ];
    for (key, value) in texts {
        if let Some(value) = value {
            entries.push((key, Any::from(value.as_str())));
        }
    }
    if let Some(created) = origin.created {
        entries.push(("created", time_any(created)?));
    }
    if !origin.used.is_empty() {
        entries.push(("used", typed_any("used", &origin.used)?));
    }
    if let Some(parameters) = &origin.parameters {
        entries.push(("parameters", json_any("parameters", parameters)?));
    }
    let mut content = MapPrelim::default();
    for (key, value) in entries {
        content.insert(Arc::from(key), In::Any(value));
    }
    txn.get_or_insert_map(ORIGINS)
        .insert(txn, origin.id.as_str(), content);
    mark_layout(txn);
    Ok(())
}

pub fn origin<T: ReadTxn>(txn: &T, id: &str) -> Option<Origin> {
    let map = object(txn, ORIGINS, id)?;
    let field = |key: &str| match map.get(txn, key) {
        Some(Out::Any(any)) => Some(any),
        _ => None,
    };
    let text_of = |key: &str| field(key).as_ref().and_then(text);
    Some(Origin {
        id: id.to_owned(),
        kind: typed(&field("kind")?)?,
        name: text_of("name").unwrap_or_default(),
        version: text_of("version"),
        author: text_of("author"),
        created: field("created").as_ref().and_then(time),
        used: field("used").as_ref().and_then(typed).unwrap_or_default(),
        parameters: field("parameters").as_ref().and_then(json),
        derived_from: text_of("derived_from"),
    })
}

pub fn comment<T: ReadTxn>(txn: &T, event: &str) -> Option<String> {
    match txn.get_map(COMMENTS)?.get(txn, event)? {
        Out::Any(any) => text(&any),
        _ => None,
    }
}

/// Sets or, with `None` or an empty text, removes the comment of an event. Returns false
/// when the room holds no such event.
pub fn set_comment(txn: &mut TransactionMut<'_>, event: &str, comment: Option<&str>) -> bool {
    if object(txn, EVENTS, event).is_none() {
        return false;
    }
    let comments: MapRef = txn.get_or_insert_map(COMMENTS);
    match comment {
        Some(comment) if !comment.is_empty() => {
            comments.insert(txn, event, comment);
        }
        _ => {
            comments.remove(txn, event);
        }
    }
    true
}
