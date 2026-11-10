//! Making any document (a notebook, a text file, a workspace manifest) equal to a past state
//! of itself, as one ordinary write: history is never rewritten, so a restore can be undone.

use std::collections::{BTreeMap, BTreeSet};

use std::time::Duration;

use similar::{Algorithm, DiffTag, TextDiff, capture_diff_slices};
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
/// maps set changed keys and remove missing ones, texts take a character diff, arrays an
/// item diff, and nested types are edited in place. With `only`, just that entry of the
/// manifest's files.
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
        (Some(want), Some(have)) => {
            if !restore_in_place(txn, past, &have, &want) {
                live.insert(txn, key, want.as_prelim(past));
            }
        }
        (Some(want), None) => {
            live.insert(txn, key, want.as_prelim(past));
        }
    }
}

/// Edits `have` until it equals `want` when both are the same shared type, keeping the live
/// object and the edits others make to it; false when `have` must be replaced.
fn restore_in_place<P: ReadTxn>(
    txn: &mut TransactionMut<'_>,
    past: &P,
    have: &Out,
    want: &Out,
) -> bool {
    match (have, want) {
        (Out::YMap(have), Out::YMap(want)) => restore_map(txn, past, have, Some(want)),
        (Out::YArray(have), Out::YArray(want)) => restore_array(txn, past, have, Some(want)),
        (Out::YText(have), Out::YText(want)) => restore_text(txn, have, &want.get_string(past)),
        _ => return have.to_json(txn) == want.to_json(past),
    }
    true
}

/// Diffs the items by their key, so kept items stay the live objects (a colleague's edit to a
/// kept cell survives, and two racing restores do not each copy it); only removed and
/// inserted runs are written. Applied from the end so the indexes still to apply stay valid.
fn restore_array<P: ReadTxn>(
    txn: &mut TransactionMut<'_>,
    past: &P,
    live: &ArrayRef,
    was: Option<&ArrayRef>,
) {
    let have: Vec<Out> = live.iter(txn).collect();
    let want: Vec<Out> = was.map(|was| was.iter(past).collect()).unwrap_or_default();
    let have_keys: Vec<Key> = have.iter().map(|item| key_of(txn, item)).collect();
    let want_keys: Vec<Key> = want.iter().map(|item| key_of(past, item)).collect();
    let ops = capture_diff_slices(Algorithm::Myers, &have_keys, &want_keys);
    for op in ops.iter().rev() {
        let (tag, old, new) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            for (at, wanted) in old.zip(new) {
                restore_item(txn, past, live, at, &have[at], &want[wanted]);
            }
            continue;
        }
        let at = index(old.start);
        if !old.is_empty() {
            live.remove_range(txn, at, index(old.len()));
        }
        for (offset, wanted) in new.enumerate() {
            live.insert(txn, at + index(offset), want[wanted].as_prelim(past));
        }
    }
}

fn restore_item<P: ReadTxn>(
    txn: &mut TransactionMut<'_>,
    past: &P,
    live: &ArrayRef,
    at: usize,
    have: &Out,
    want: &Out,
) {
    if !restore_in_place(txn, past, have, want) {
        live.remove(txn, index(at));
        live.insert(txn, index(at), want.as_prelim(past));
    }
}

/// What an item is matched on across versions: a cell's `id`, else its content.
#[derive(Debug, PartialEq, Eq, Hash)]
enum Key {
    Id(String),
    Content(String),
}

fn key_of<T: ReadTxn>(txn: &T, item: &Out) -> Key {
    if let Out::YMap(map) = item
        && let Some(Out::Any(Any::String(id))) = map.get(txn, "id")
    {
        return Key::Id(id.to_string());
    }
    let mut content = String::new();
    item.to_json(txn).to_json(&mut content);
    Key::Content(content)
}

fn index(at: usize) -> u32 {
    u32::try_from(at).unwrap_or(u32::MAX)
}

/// Past it the diff settles for a coarser, still correct edit: a huge, very different text
/// cannot hold the room's worker.
const TEXT_DIFF_TIMEOUT: Duration = Duration::from_secs(1);

/// One character diff of the live text against the past one, applied from the end so the
/// offsets still to apply stay valid.
fn restore_text(txn: &mut TransactionMut<'_>, live: &TextRef, was: &str) {
    let now = live.get_string(txn);
    let at = offsets(&now, txn.doc().offset_kind());
    let wanted = char_starts(was);
    let diff = TextDiff::configure()
        .timeout(TEXT_DIFF_TIMEOUT)
        .diff_chars(now.as_str(), was);
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
