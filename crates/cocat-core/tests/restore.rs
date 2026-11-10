//! Making any document (a notebook, a text file, a workspace manifest) equal to a past state
//! of itself.

use cocat_core::restore::{document_json, restore_document};
use serde_json::Value;
use yrs::updates::decoder::Decode;
use yrs::{
    Array, ArrayPrelim, Doc, GetString, Map, MapPrelim, ReadTxn, StateVector, Text, TextPrelim,
    Transact, TransactionMut, Update, WriteTxn,
};

/// A copy made the way the server makes one, from updates: its root types are unnamed.
fn replica(doc: &Doc) -> Doc {
    let state = doc
        .transact()
        .encode_state_as_update_v1(&StateVector::default());
    let copy = Doc::new();
    copy.transact_mut()
        .apply_update(Update::decode_v1(&state).unwrap())
        .unwrap();
    copy
}

fn json(doc: &Doc) -> Value {
    document_json(doc).unwrap()
}

fn cell(id: &str, source: &str) -> MapPrelim {
    MapPrelim::from([
        ("id".to_owned(), yrs::In::from(id)),
        ("cell_type".to_owned(), yrs::In::from("code")),
        (
            "source".to_owned(),
            yrs::In::Text(TextPrelim::new(source).into()),
        ),
        ("outputs".to_owned(), yrs::In::Array(ArrayPrelim::default())),
    ])
}

/// A YNotebook: `meta`, `state` maps and `cells` of maps whose `source` is a text.
fn notebook() -> Doc {
    let doc = Doc::new();
    let mut txn = doc.transact_mut();
    txn.get_or_insert_map("meta")
        .insert(&mut txn, "nbformat", 4);
    txn.get_or_insert_map("state")
        .insert(&mut txn, "dirty", false);
    let cells = txn.get_or_insert_array("cells");
    cells.push_back(&mut txn, cell("a", "print(1)\nprint(2)"));
    cells.push_back(&mut txn, cell("b", "x = 2"));
    drop(txn);
    doc
}

fn restored(live: &Doc, past: &Doc, only: Option<&str>) {
    let mut txn = live.transact_mut();
    restore_document(&mut txn, past, only);
}

fn edit_first_source(txn: &mut TransactionMut, at: u32, text: &str) {
    let cells = txn.get_or_insert_array("cells");
    let Some(yrs::Out::YMap(first)) = cells.get(txn, 0) else {
        panic!("no first cell")
    };
    let Some(yrs::Out::YText(source)) = first.get(txn, "source") else {
        panic!("no source")
    };
    source.insert(txn, at, text);
}

#[test]
fn restoring_brings_back_deleted_cells_and_text() {
    let past = replica(&notebook());
    let live = replica(&past);
    {
        let mut txn = live.transact_mut();
        edit_first_source(&mut txn, 8, " # changed");
        txn.get_or_insert_array("cells").remove(&mut txn, 1);
        txn.get_or_insert_map("meta")
            .insert(&mut txn, "kernel", "python3");
        txn.get_or_insert_map("state").remove(&mut txn, "dirty");
    }
    let live = replica(&live);
    assert_ne!(json(&live), json(&past));

    restored(&live, &past, None);

    assert_eq!(json(&live), json(&past));
    let other = replica(&live);
    assert_eq!(json(&other), json(&past));
}

#[test]
fn restoring_a_text_file_edits_only_what_changed() {
    let past = Doc::new();
    {
        let mut txn = past.transact_mut();
        txn.get_or_insert_text("source")
            .insert(&mut txn, 0, "héllo wörld\nsecond line\n");
        txn.get_or_insert_map("state")
            .insert(&mut txn, "dirty", false);
    }
    let past = replica(&past);
    let live = replica(&past);
    {
        let mut txn = live.transact_mut();
        let source = txn.get_or_insert_text("source");
        source.remove_range(&mut txn, 0, 7);
        source.insert(&mut txn, 0, "bye ");
    }
    let live = replica(&live);
    let concurrent = replica(&live);
    {
        let mut txn = concurrent.transact_mut();
        let source = txn.get_or_insert_text("source");
        let text = source.get_string(&txn);
        let after_second = text.find("second").unwrap() + "second".len();
        source.insert(&mut txn, u32::try_from(after_second).unwrap(), " LINE");
    }

    restored(&live, &past, None);
    let update = live
        .transact()
        .encode_state_as_update_v1(&concurrent.transact().state_vector());
    concurrent
        .transact_mut()
        .apply_update(Update::decode_v1(&update).unwrap())
        .unwrap();

    let txn = live.transact();
    assert_eq!(
        txn.get_text("source").unwrap().get_string(&txn),
        "héllo wörld\nsecond line\n"
    );
    let txn = concurrent.transact();
    assert_eq!(
        txn.get_text("source").unwrap().get_string(&txn),
        "héllo wörld\nsecond LINE line\n",
        "a rewrite of the whole text would have moved the concurrent edit to the end"
    );
}

fn manifest_entry(kind: &str, size: i64) -> MapPrelim {
    MapPrelim::from([
        ("kind".to_owned(), yrs::In::from(kind)),
        ("size".to_owned(), yrs::In::from(size)),
        ("by".to_owned(), yrs::In::from("alice@x.org")),
    ])
}

#[test]
fn restoring_one_manifest_path_leaves_the_others() {
    let past = Doc::new();
    {
        let mut txn = past.transact_mut();
        let files = txn.get_or_insert_map("files");
        files.insert(&mut txn, "a.ipynb", manifest_entry("notebook", 1));
        files.insert(&mut txn, "b.txt", manifest_entry("text", 2));
        txn.get_or_insert_map("rules")
            .insert(&mut txn, "patterns", TextPrelim::new("*.tmp\n"));
    }
    let past = replica(&past);
    let live = replica(&past);
    {
        let mut txn = live.transact_mut();
        let files = txn.get_or_insert_map("files");
        files.insert(&mut txn, "a.ipynb", manifest_entry("notebook", 10));
        files.remove(&mut txn, "b.txt");
        files.insert(&mut txn, "c.csv", manifest_entry("file", 3));
        txn.get_or_insert_map("rules")
            .insert(&mut txn, "patterns", TextPrelim::new("*.log\n"));
    }
    let live = replica(&live);
    let before = json(&live);

    restored(&live, &past, Some("b.txt"));

    let after = json(&live);
    let past_json = json(&past);
    assert_eq!(after["files"]["b.txt"], past_json["files"]["b.txt"]);
    assert_eq!(after["files"]["a.ipynb"], before["files"]["a.ipynb"]);
    assert_eq!(after["files"]["c.csv"], before["files"]["c.csv"]);
    assert_eq!(after["rules"], before["rules"]);

    restored(&live, &past, Some("c.csv"));

    assert!(json(&live)["files"].get("c.csv").is_none());
}

#[test]
fn restoring_the_present_writes_nothing() {
    let past = replica(&notebook());
    let live = replica(&past);
    let before = live.transact().state_vector();

    restored(&live, &past, None);

    assert_eq!(live.transact().state_vector(), before);
}

#[test]
fn the_json_of_a_document_names_its_roots() {
    let json = json(&replica(&notebook()));

    assert_eq!(json["meta"]["nbformat"], 4);
    assert_eq!(json["cells"][1]["source"], "x = 2");
    assert_eq!(json["state"]["dirty"], false);
}
