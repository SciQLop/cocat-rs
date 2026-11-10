//! Making any document (a notebook, a text file, a workspace manifest) equal to a past state
//! of itself, as one ordinary write: history is never rewritten, so a restore can be undone.

use std::collections::{BTreeMap, BTreeSet};

use similar::{DiffTag, TextDiff};
use yrs::types::{AsPrelim, ToJson};
use yrs::{
    Any, Array, ArrayRef, Doc, GetString, In, Map, MapRef, OffsetKind, Out, ReadTxn, Text, TextRef,
    Transact, TransactionMut, WriteTxn,
};

/// The manifest's root map of files, keyed by path.
pub const MANIFEST_FILES: &str = "files";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Map,
    Array,
    Text,
}

/// Makes every root type of the live document equal to `past`'s, writing only what differs:
/// maps set changed keys and remove missing ones, nested maps and texts are edited in place,
/// arrays are rebuilt. With `only`, just that entry of the manifest's files.
pub fn restore_document(txn: &mut TransactionMut<'_>, past: &Doc, only: Option<&str>) {
    let past = past.transact();
    match only {
        Some(path) => {
            let files = txn.get_or_insert_map(MANIFEST_FILES);
            let was = past.get_map(MANIFEST_FILES);
            restore_entry(txn, &past, &files, was.as_ref(), path);
        }
        None => restore_roots(txn, &past),
    }
}

/// The document's root types as JSON, by name; empty roots are left out.
pub fn document_json(doc: &Doc) -> Result<serde_json::Value, serde_json::Error> {
    let txn = doc.transact();
    let roots: BTreeMap<String, Any> = root_kinds(&txn)
        .into_iter()
        .filter_map(|(name, kind)| Some((name.clone(), root_json(&txn, &name, kind)?)))
        .collect();
    serde_json::to_value(roots)
}

fn root_json<T: ReadTxn>(txn: &T, name: &str, kind: Kind) -> Option<Any> {
    match kind {
        Kind::Map => Some(txn.get_map(name)?.to_json(txn)),
        Kind::Array => Some(txn.get_array(name)?.to_json(txn)),
        Kind::Text => Some(Any::from(txn.get_text(name)?.get_string(txn))),
    }
}

fn restore_roots<P: ReadTxn>(txn: &mut TransactionMut<'_>, past: &P) {
    let mut kinds = root_kinds(past);
    kinds.extend(root_kinds(txn));
    for (name, kind) in kinds {
        let name = name.as_str();
        match kind {
            Kind::Map => {
                let live = txn.get_or_insert_map(name);
                restore_map(txn, past, &live, past.get_map(name).as_ref());
            }
            Kind::Array => {
                let live = txn.get_or_insert_array(name);
                restore_array(txn, past, &live, past.get_array(name).as_ref());
            }
            Kind::Text => {
                let live = txn.get_or_insert_text(name);
                let was = past.get_text(name).map(|text| text.get_string(past));
                restore_text(txn, &live, was.as_deref().unwrap_or_default());
            }
        }
    }
}

/// The kinds of the roots that hold something, the live kind winning. A document built
/// from updates does not know its roots' types until they are named, so they are read from
/// the content, the way yrs itself guesses them.
// simplify: the kind is read from a full copy of each root; fine for documents of a few MB,
// look at the content's first item instead if large documents make restores slow.
fn root_kinds<T: ReadTxn>(txn: &T) -> BTreeMap<String, Kind> {
    txn.root_refs()
        .filter_map(|(name, root)| Some((name.to_owned(), kind_of(txn, &root)?)))
        .collect()
}

fn kind_of<T: ReadTxn>(txn: &T, root: &Out) -> Option<Kind> {
    match root.as_prelim(txn) {
        In::Map(map) if !map.is_empty() => Some(Kind::Map),
        In::Array(array) if !array.is_empty() => Some(Kind::Array),
        In::Text(text) if !text.is_empty() => Some(Kind::Text),
        _ => None,
    }
}

fn restore_map<P: ReadTxn>(
    txn: &mut TransactionMut<'_>,
    past: &P,
    live: &MapRef,
    was: Option<&MapRef>,
) {
    let mut keys: BTreeSet<String> = live.keys(txn).map(str::to_owned).collect();
    if let Some(was) = was {
        keys.extend(was.keys(past).map(str::to_owned));
    }
    for key in keys {
        restore_entry(txn, past, live, was, &key);
    }
}

fn restore_entry<P: ReadTxn>(
    txn: &mut TransactionMut<'_>,
    past: &P,
    live: &MapRef,
    was: Option<&MapRef>,
    key: &str,
) {
    let want = was.and_then(|was| was.get(past, key));
    match (want, live.get(txn, key)) {
        (None, None) => {}
        (None, Some(_)) => {
            live.remove(txn, key);
        }
        (Some(Out::YMap(want)), Some(Out::YMap(have))) => {
            restore_map(txn, past, &have, Some(&want));
        }
        (Some(Out::YText(want)), Some(Out::YText(have))) => {
            restore_text(txn, &have, &want.get_string(past));
        }
        (Some(want), Some(have)) if want.to_json(past) == have.to_json(txn) => {}
        (Some(want), _) => {
            live.insert(txn, key, want.as_prelim(past));
        }
    }
}

fn restore_array<P: ReadTxn>(
    txn: &mut TransactionMut<'_>,
    past: &P,
    live: &ArrayRef,
    was: Option<&ArrayRef>,
) {
    let wanted: Vec<Out> = was.map(|was| was.iter(past).collect()).unwrap_or_default();
    let have: Vec<Any> = live.iter(txn).map(|value| value.to_json(txn)).collect();
    let want: Vec<Any> = wanted.iter().map(|value| value.to_json(past)).collect();
    if have == want {
        return;
    }
    let len = live.len(txn);
    live.remove_range(txn, 0, len);
    for value in wanted {
        live.push_back(txn, value.as_prelim(past));
    }
}

/// One character diff of the live text against the past one, applied from the end so the
/// offsets still to apply stay valid.
fn restore_text(txn: &mut TransactionMut<'_>, live: &TextRef, was: &str) {
    let now = live.get_string(txn);
    let at = offsets(&now, txn.doc().offset_kind());
    let wanted = char_starts(was);
    let diff = TextDiff::from_chars(now.as_str(), was);
    for op in diff.ops().iter().rev() {
        let (tag, old, new) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            continue;
        }
        let start = at[old.start];
        if old.end > old.start {
            live.remove_range(txn, start, at[old.end] - start);
        }
        if new.end > new.start {
            live.insert(txn, start, &was[wanted[new.start]..wanted[new.end]]);
        }
    }
}

/// Where each character starts in the document's offset unit, then where the text ends.
fn offsets(text: &str, kind: OffsetKind) -> Vec<u32> {
    let width = |c: char| match kind {
        OffsetKind::Bytes => c.len_utf8(),
        OffsetKind::Utf16 => c.len_utf16(),
    };
    let mut at = vec![0u32];
    for c in text.chars() {
        let last = at.last().copied().unwrap_or_default();
        at.push(last.saturating_add(u32::try_from(width(c)).unwrap_or(u32::MAX)));
    }
    at
}

fn char_starts(text: &str) -> Vec<usize> {
    text.char_indices()
        .map(|(index, _)| index)
        .chain([text.len()])
        .collect()
}
