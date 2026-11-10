//! Writing a SciQLop workspace's files: notebooks as JupyterLab writes them, text files,
//! manifest entries, and what is left out of the sharing.

use std::collections::BTreeMap;

use cocat_core::files::{
    DEFAULT_PATTERNS, FileKind, ManifestEntry, Plan, Rules, Skip, patterns, plan, put_entry,
    set_patterns, write_notebook, write_text,
};
use serde_json::{Value, json};
use yrs::Out;
use yrs::types::ToJson;
use yrs::updates::decoder::Decode;
use yrs::{Array, Doc, GetString, Map, ReadTxn, Transact, Update};

const FIXTURE: &[u8] = include_bytes!("fixtures/ynotebook.bin");
const FIXTURE_JSON: &str = include_str!("fixtures/ynotebook.json");
const BARE: &[u8] = include_bytes!("fixtures/ynotebook_bare.bin");
const BARE_JSON: &str = include_str!("fixtures/ynotebook_bare.json");

/// The wire may hold a whole number as an integer or a float; Yjs reads both as one number.
fn floats(value: Value) -> Value {
    match value {
        Value::Number(n) => json!(n.as_f64()),
        Value::Array(items) => Value::Array(items.into_iter().map(floats).collect()),
        Value::Object(fields) => {
            Value::Object(fields.into_iter().map(|(k, v)| (k, floats(v))).collect())
        }
        other => other,
    }
}

/// A document's value with its Y types kept: `{"map": ...}`, `{"array": ...}`,
/// `{"text": ...}`, `{"any": ...}`.
fn shape<T: ReadTxn>(txn: &T, out: Out) -> Value {
    match out {
        Out::YMap(map) => {
            let entries: BTreeMap<String, Value> = map
                .iter(txn)
                .map(|(key, value)| (key.to_owned(), shape(txn, value)))
                .collect();
            json!({ "map": entries })
        }
        Out::YArray(array) => {
            let items: Vec<Value> = array.iter(txn).map(|item| shape(txn, item)).collect();
            json!({ "array": items })
        }
        Out::YText(text) => json!({ "text": text.get_string(txn) }),
        Out::Any(any) => json!({ "any": floats(serde_json::to_value(any).unwrap()) }),
        other => panic!("unexpected {other:?}"),
    }
}

/// The notebook roots by name. A document that came from an update does not know its root
/// types yet, so they are named here: `meta` is a map and `cells` an array.
fn roots(doc: &Doc) -> BTreeMap<String, Value> {
    let names: Vec<String> = doc
        .transact()
        .root_refs()
        .map(|(name, _)| name.to_owned())
        .collect();
    assert!(
        names.iter().all(|name| name == "meta" || name == "cells"),
        "{names:?}"
    );
    let (meta, cells) = (
        doc.get_or_insert_map("meta"),
        doc.get_or_insert_array("cells"),
    );
    let txn = doc.transact();
    BTreeMap::from([
        ("meta".to_owned(), shape(&txn, Out::YMap(meta))),
        ("cells".to_owned(), shape(&txn, Out::YArray(cells))),
    ])
}

fn files_json<T: ReadTxn>(txn: &T) -> Value {
    let files = txn.get_map("files").unwrap().to_json(txn);
    floats(serde_json::to_value(files).unwrap())
}

fn fixture_notebook() -> Value {
    serde_json::from_str(FIXTURE_JSON).unwrap()
}

fn written(notebook: &Value) -> Doc {
    let doc = Doc::new();
    write_notebook(&mut doc.transact_mut(), notebook).unwrap();
    doc
}

fn assert_written_as_jupyterlab(update: &[u8], notebook: &str, cells: usize) {
    let jupyterlab = Doc::new();
    jupyterlab
        .transact_mut()
        .apply_update(Update::decode_v1(update).unwrap())
        .unwrap();
    let ours = written(&serde_json::from_str(notebook).unwrap());
    let (expected, actual) = (roots(&jupyterlab), roots(&ours));
    assert_eq!(
        expected["cells"]["array"].as_array().map(Vec::len),
        Some(cells)
    );
    for (name, root) in &expected {
        assert_eq!(&actual[name], root, "root {name}");
    }
}

#[test]
fn a_notebook_is_written_as_jupyterlab_writes_it() {
    assert_written_as_jupyterlab(FIXTURE, FIXTURE_JSON, 5);
}

#[test]
fn the_defaults_of_a_bare_notebook_are_jupyterlabs() {
    assert_written_as_jupyterlab(BARE, BARE_JSON, 1);
}

#[test]
fn a_cell_without_an_id_gets_one() {
    let doc = written(&json!({"cells": [
        {"cell_type": "markdown", "metadata": {}, "source": "a"},
        {"cell_type": "markdown", "metadata": {}, "source": "b"}
    ]}));
    let cells = roots(&doc)["cells"]["array"].clone();
    let ids: Vec<&str> = cells
        .as_array()
        .unwrap()
        .iter()
        .map(|cell| cell["map"]["id"]["any"].as_str().unwrap())
        .collect();
    assert!(ids.iter().all(|id| uuid::Uuid::parse_str(id).is_ok()));
    assert_ne!(ids[0], ids[1]);
}

#[test]
fn an_empty_notebook_gets_one_empty_code_cell() {
    let doc = written(&json!({"cells": []}));
    let cells = roots(&doc)["cells"]["array"].clone();
    let [cell] = cells.as_array().unwrap().as_slice() else {
        panic!("one cell expected");
    };
    let cell = &cell["map"];
    assert_eq!(cell["cell_type"], json!({"any": "code"}));
    assert_eq!(cell["source"], json!({"text": ""}));
    assert_eq!(cell["outputs"], json!({"array": []}));
    assert_eq!(cell["execution_count"], json!({"any": null}));
    assert_eq!(cell["metadata"], json!({"map": {"trusted": {"any": true}}}));
}

#[test]
fn writing_again_replaces_the_notebook() {
    let doc = written(&fixture_notebook());
    let other = json!({"nbformat": 4, "nbformat_minor": 5, "metadata": {}, "cells": [
        {"id": "x", "cell_type": "raw", "metadata": {}, "source": "only"}
    ]});
    write_notebook(&mut doc.transact_mut(), &other).unwrap();
    assert_eq!(roots(&doc), roots(&written(&other)));
}

#[test]
fn a_notebook_that_is_not_one_is_refused() {
    let doc = Doc::new();
    for bad in [json!([]), json!({}), json!({"cells": [{"source": "x"}]})] {
        assert!(
            write_notebook(&mut doc.transact_mut(), &bad).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn a_text_file_is_its_source() {
    let doc = Doc::new();
    write_text(&mut doc.transact_mut(), "first version");
    write_text(&mut doc.transact_mut(), "print(1)\n");
    let txn = doc.transact();
    assert_eq!(
        txn.get_text("source").unwrap().get_string(&txn),
        "print(1)\n"
    );
}

#[test]
fn a_manifest_entry_names_its_document_and_author() {
    let doc = Doc::new();
    let entry = ManifestEntry {
        kind: FileKind::Notebook,
        doc: Some("0b6f3e7c-2a41-4d8e-9c55-7f1a2b3c4d5e".into()),
        hash: None,
        size: 1234,
        by: Some("alice".into()),
        at: 1_700_000_000_000,
    };
    put_entry(&mut doc.transact_mut(), "nb/a.ipynb", &entry);
    let txn = doc.transact();
    let files = files_json(&txn);
    assert_eq!(
        files,
        json!({"nb/a.ipynb": {
            "kind": "notebook",
            "doc": "0b6f3e7c-2a41-4d8e-9c55-7f1a2b3c4d5e",
            "size": 1234.0,
            "by": "alice",
            "at": 1_700_000_000_000.0
        }})
    );
}

#[test]
fn a_whole_file_entry_carries_its_hash_and_no_document() {
    let doc = Doc::new();
    let entry = ManifestEntry {
        kind: FileKind::File,
        doc: None,
        hash: Some("abc".into()),
        size: 3,
        by: None,
        at: 5,
    };
    put_entry(&mut doc.transact_mut(), "p.png", &entry);
    let txn = doc.transact();
    let files = files_json(&txn);
    assert_eq!(
        files,
        json!({"p.png": {"kind": "file", "hash": "abc", "size": 3.0, "at": 5.0}})
    );
}

#[test]
fn the_sharing_patterns_are_kept_in_the_manifest() {
    let doc = Doc::new();
    assert_eq!(patterns(&doc.transact()), None);
    set_patterns(&mut doc.transact_mut(), "*.log\n");
    assert_eq!(patterns(&doc.transact()).as_deref(), Some("*.log\n"));
}

#[test]
fn the_default_rules_leave_out_caches_and_data() {
    let rules = Rules::new(DEFAULT_PATTERNS);
    for left_out in [
        ".git/config",
        "src/__pycache__/m.pyc",
        "nb/.ipynb_checkpoints/a-checkpoint.ipynb",
        ".venv/bin/python",
        "build/out.o",
        "dist/pkg.whl",
        "x.tmp",
        "notes~",
        "d/.DS_Store",
        "data/mms.cdf",
        "a.h5",
        "a.hdf5",
        "a.nc",
        "a.fits",
        "a.npy",
        "a.npz",
        "a.parquet",
        "a.pkl",
    ] {
        assert!(rules.leaves_out(left_out), "{left_out}");
    }
    for kept in ["analysis.ipynb", "src/main.py", "figs/a.png", "README.md"] {
        assert!(!rules.leaves_out(kept), "{kept}");
    }
}

#[test]
fn a_leading_slash_does_not_matter() {
    let rules = Rules::new(DEFAULT_PATTERNS);
    assert!(rules.leaves_out("/data/a.cdf"));
    assert!(!rules.leaves_out("/a.py"));
}

#[test]
fn a_file_left_out_and_too_big_is_left_out_by_the_rules() {
    let rules = Rules::new("*.cdf");
    assert_eq!(plan("a.cdf", 99, &rules, 10), Plan::Skip(Skip::Rules));
}

#[test]
fn a_forced_path_comes_back() {
    let rules = Rules::new("*.cdf\n!keep/ref.cdf\n");
    assert!(rules.leaves_out("a.cdf"));
    assert!(!rules.leaves_out("keep/ref.cdf"));
}

#[test]
fn a_file_over_the_cap_is_skipped() {
    let rules = Rules::new("");
    assert_eq!(
        plan("big.bin", 11, &rules, 10),
        Plan::Skip(Skip::TooBig { cap: 10 })
    );
    assert_eq!(
        plan("big.ipynb", 11, &rules, 10),
        Plan::Skip(Skip::TooBig { cap: 10 })
    );
    assert_eq!(plan("ok.bin", 10, &rules, 10), Plan::File);
}

#[test]
fn a_notebook_a_script_and_a_picture_are_sorted() {
    let rules = Rules::new(DEFAULT_PATTERNS);
    assert_eq!(plan("a/n.ipynb", 1, &rules, 100), Plan::Notebook);
    assert_eq!(plan("a/N.IPYNB", 1, &rules, 100), Plan::Notebook);
    assert_eq!(plan("a/s.py", 1, &rules, 100), Plan::Text);
    assert_eq!(plan("a/p.png", 1, &rules, 100), Plan::File);
    assert_eq!(plan("a/Makefile", 1, &rules, 100), Plan::File);
    assert_eq!(plan("a/d.cdf", 1, &rules, 100), Plan::Skip(Skip::Rules));
}
