//! Writing a SciQLop workspace's files into documents, and deciding which files to share.
//!
//! The notebook layout is the one `jupyter_ydoc` writes (what JupyterLab and SciQLop open),
//! the text layout a single root text, and the manifest a `files` map of entries next to a
//! `rules` map holding the sharing patterns.

use std::collections::BTreeMap;
use std::sync::Arc;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde_json::Value;
use yrs::{
    Any, Array, ArrayPrelim, In, Map, MapPrelim, Out, ReadTxn, Text, TextPrelim, TransactionMut,
    WriteTxn,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Notebook,
    Text,
    File,
}

impl FileKind {
    fn as_str(self) -> &'static str {
        match self {
            FileKind::Notebook => "notebook",
            FileKind::Text => "text",
            FileKind::File => "file",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("not a notebook: it has no list of cells")]
    NoCells,
    #[error("cell {0} is not a notebook cell")]
    BadCell(usize),
}

/// Extension to syntect lookup token; `None` means plain text. The files the viewer shows as
/// text, and so the ones an upload turns into live documents.
pub const TEXTS: &[(&str, Option<&str>)] = &[
    ("py", Some("py")),
    ("md", Some("md")),
    ("txt", None),
    ("toml", Some("toml")),
    ("json", Some("json")),
    ("yaml", Some("yaml")),
    ("yml", Some("yaml")),
    ("cfg", Some("ini")),
    ("ini", Some("ini")),
    ("csv", None),
    ("sh", Some("sh")),
    ("rs", Some("rs")),
    ("c", Some("c")),
    ("cpp", Some("cpp")),
    ("h", Some("c")),
    ("js", Some("js")),
    ("ts", Some("ts")),
    ("html", Some("html")),
    ("css", Some("css")),
    ("tex", Some("tex")),
];

/// The lowercase extension of a path's file name, empty when it has none.
pub fn extension(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.')
        .map(|(_, ext)| ext.to_lowercase())
        .unwrap_or_default()
}

pub fn is_text(path: &str) -> bool {
    let ext = extension(path);
    TEXTS.iter().any(|(known, _)| *known == ext)
}

fn number(n: f64) -> In {
    In::Any(Any::from(n))
}

/// Yjs numbers are all floats, so every JSON number is stored as one.
fn any(value: &Value) -> Any {
    match value {
        Value::Null => Any::Null,
        Value::Bool(b) => Any::Bool(*b),
        Value::Number(n) => Any::from(n.as_f64().unwrap_or_default()),
        Value::String(s) => Any::String(s.as_str().into()),
        Value::Array(items) => Any::Array(items.iter().map(any).collect()),
        Value::Object(fields) => Any::Map(Arc::new(
            fields.iter().map(|(k, v)| (k.clone(), any(v))).collect(),
        )),
    }
}

/// nbformat allows a text field to be a string or a list of strings.
fn joined(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

fn text_in(value: Option<&Value>) -> In {
    In::from(TextPrelim::new(joined(value)))
}

fn map_in(entries: impl IntoIterator<Item = (String, In)>) -> In {
    In::Map(MapPrelim::from_iter(entries))
}

fn any_entries(value: Option<&Value>) -> impl Iterator<Item = (String, In)> {
    value
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(key, value)| (key.clone(), In::Any(any(value))))
}

fn output_in(output: &Value, index: usize) -> Result<In, FileError> {
    let fields = output.as_object().ok_or(FileError::BadCell(index))?;
    let mut entries: BTreeMap<String, In> = any_entries(Some(output)).collect();
    if fields.get("output_type").and_then(Value::as_str) == Some("stream") {
        entries.insert("text".into(), text_in(fields.get("text")));
    }
    Ok(map_in(entries))
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(fields) => fields.is_empty(),
        Value::String(s) => s.is_empty(),
        _ => false,
    }
}

fn cell_in(cell: &Value, index: usize) -> Result<In, FileError> {
    let kind = cell
        .get("cell_type")
        .and_then(Value::as_str)
        .ok_or(FileError::BadCell(index))?;
    let mut entries: BTreeMap<String, In> = any_entries(Some(cell)).collect();
    entries
        .entry("id".into())
        .or_insert_with(|| In::Any(Any::String(uuid::Uuid::new_v4().to_string().into())));
    entries.insert("source".into(), text_in(cell.get("source")));
    entries.insert("metadata".into(), map_in(any_entries(cell.get("metadata"))));
    match kind {
        "code" => {
            let outputs = cell.get("outputs").and_then(Value::as_array);
            let outputs = outputs
                .into_iter()
                .flatten()
                .map(|output| output_in(output, index))
                .collect::<Result<Vec<_>, _>>()?;
            entries.insert("outputs".into(), In::Array(ArrayPrelim::from_iter(outputs)));
            entries.insert(
                "execution_state".into(),
                In::Any(Any::String("idle".into())),
            );
        }
        "markdown" | "raw" if cell.get("attachments").is_some_and(is_empty) => {
            entries.remove("attachments");
        }
        _ => {}
    }
    Ok(map_in(entries))
}

fn empty_cell() -> Value {
    serde_json::json!({
        "cell_type": "code",
        "execution_count": null,
        "metadata": {"trusted": true},
        "outputs": [],
        "source": "",
    })
}

fn meta_entries(notebook: &Value) -> Vec<(String, In)> {
    let number_or = |key: &str, default: f64| {
        number(notebook.get(key).and_then(Value::as_f64).unwrap_or(default))
    };
    let mut metadata: BTreeMap<String, In> = any_entries(notebook.get("metadata")).collect();
    metadata
        .entry("language_info".into())
        .or_insert_with(|| In::Any(any(&serde_json::json!({"name": ""}))));
    metadata
        .entry("kernelspec".into())
        .or_insert_with(|| In::Any(any(&serde_json::json!({"name": "", "display_name": ""}))));
    vec![
        ("nbformat".into(), number_or("nbformat", 4.0)),
        ("nbformat_minor".into(), number_or("nbformat_minor", 5.0)),
        ("metadata".into(), map_in(metadata)),
    ]
}

/// Replaces whatever the document held with the notebook, laid out as `jupyter_ydoc` lays it
/// out. A cell without an id gets one; an empty notebook gets one empty code cell. A notebook
/// that cannot be written leaves the document untouched.
pub fn write_notebook(txn: &mut TransactionMut, notebook: &Value) -> Result<(), FileError> {
    let cells = notebook
        .get("cells")
        .and_then(Value::as_array)
        .ok_or(FileError::NoCells)?;
    let placeholder = [empty_cell()];
    let cells = if cells.is_empty() {
        &placeholder[..]
    } else {
        cells
    };
    let cells = cells
        .iter()
        .enumerate()
        .map(|(index, cell)| cell_in(cell, index))
        .collect::<Result<Vec<_>, _>>()?;

    let meta = txn.get_or_insert_map("meta");
    meta.clear(txn);
    for (key, value) in meta_entries(notebook) {
        meta.insert(txn, key, value);
    }
    let ycells = txn.get_or_insert_array("cells");
    let old = ycells.len(txn);
    ycells.remove_range(txn, 0, old);
    for cell in cells {
        ycells.push_back(txn, cell);
    }
    Ok(())
}

/// Replaces the document's text.
pub fn write_text(txn: &mut TransactionMut, text: &str) {
    let source = txn.get_or_insert_text("source");
    let old = source.len(txn);
    source.remove_range(txn, 0, old);
    source.insert(txn, 0, text);
}

/// One line of a manifest: what a path holds and who put it there. A notebook or a text file
/// names its document; a whole file names the hash of its stored bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    pub kind: FileKind,
    pub doc: Option<String>,
    pub hash: Option<String>,
    pub size: u64,
    pub by: Option<String>,
    /// Milliseconds since the epoch.
    pub at: i64,
}

pub fn put_entry(txn: &mut TransactionMut, path: &str, entry: &ManifestEntry) {
    let text = |value: &str| In::Any(Any::String(value.into()));
    let optional = |key: &str, value: &Option<String>| {
        value.as_deref().map(|value| (key.to_owned(), text(value)))
    };
    let entries = [
        Some(("kind".to_owned(), text(entry.kind.as_str()))),
        optional("doc", &entry.doc),
        optional("hash", &entry.hash),
        Some(("size".to_owned(), number(entry.size as f64))),
        optional("by", &entry.by),
        Some(("at".to_owned(), number(entry.at as f64))),
    ];
    let files = txn.get_or_insert_map("files");
    files.insert(
        txn,
        path,
        MapPrelim::from_iter(entries.into_iter().flatten()),
    );
}

pub const DEFAULT_PATTERNS: &str = ".git/\n__pycache__/\n.ipynb_checkpoints/\n.venv/\nbuild/\n\
dist/\n*.tmp\n*~\n.DS_Store\n*.cdf\n*.h5\n*.hdf5\n*.nc\n*.fits\n*.npy\n*.npz\n*.parquet\n*.pkl\n";

pub fn set_patterns(txn: &mut TransactionMut, text: &str) {
    txn.get_or_insert_map("rules").insert(txn, "patterns", text);
}

pub fn patterns<T: ReadTxn>(txn: &T) -> Option<String> {
    match txn.get_map("rules")?.get(txn, "patterns")? {
        Out::Any(Any::String(text)) => Some(text.to_string()),
        _ => None,
    }
}

/// Sharing rules in `.gitignore` syntax: what they match is left out, `!path` brings it back.
pub struct Rules(Gitignore);

impl Rules {
    pub fn new(patterns: &str) -> Self {
        let mut builder = GitignoreBuilder::new("");
        for line in patterns.lines() {
            // A line that is not a valid pattern is dropped: it leaves nothing out.
            let _ = builder.add_line(None, line);
        }
        Rules(builder.build().unwrap_or_else(|_| Gitignore::empty()))
    }

    /// Whether `path` is left out, itself or through a folder it is in.
    pub fn leaves_out(&self, path: &str) -> bool {
        self.0.matched_path_or_any_parents(path, false).is_ignore()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    Rules,
    TooBig { cap: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    Notebook,
    Text,
    File,
    Skip(Skip),
}

/// What to do with a file about to be uploaded: skip it, or the kind of document it becomes.
pub fn plan(path: &str, size: u64, rules: &Rules, cap: u64) -> Plan {
    if rules.leaves_out(path) {
        Plan::Skip(Skip::Rules)
    } else if size > cap {
        Plan::Skip(Skip::TooBig { cap })
    } else if extension(path) == "ipynb" {
        Plan::Notebook
    } else if is_text(path) {
        Plan::Text
    } else {
        Plan::File
    }
}
