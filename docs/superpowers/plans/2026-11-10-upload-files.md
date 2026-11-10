# Upload a SciQLop workspace's files from the web app — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An editor uploads files, or a folder, into a project from the web app. Notebooks and
text files become live documents SciQLop can edit together; other files are stored whole. A
project without files gets its file list on the first upload.

**Architecture:** The core learns to write what the viewer already reads: a notebook in
JupyterLab's own layout (`jupyter_ydoc` 4.x `YNotebook`), a text file (`YUnicode`), a
manifest entry, and the sharing rules (`.gitignore` syntax). The web app reads the picked
files, sorts them (live document, whole file, skipped with a reason), creates the manifest
when missing, writes each document over its own socket (`/room/{project}/{doc}`), PUTs
blobs, then writes the manifest entries. The server only gains one meta tag (the file size
cap).

**Tech Stack:** Rust (cocat-core, cocat-web/Leptos, cocat-server templates), yrs; crates
`ignore` (gitignore matching, `ignore::gitignore::GitignoreBuilder`) and `sha2` (already in
the lock file).

**Spec:** `docs/superpowers/specs/2026-11-10-workspace-sharing-design.md` ("Storage",
"Sharing rules") and the user's request (2026-11-10): "being able to upload files through
the web ui would be nice", agreed with: drop files or use "Upload files…", same sharing rules
as SciQLop, list what was skipped and why, whole files last-writer-wins, notebooks and text
become live documents.

## Global Constraints

- No change outside cocat-rs (no SciQLop or plugin changes yet).
- Notebook layout = `jupyter_ydoc` 4.2.1 `YNotebook.set` on an empty doc: root map `meta`
  {`nbformat`, `nbformat_minor`, `metadata` (a Y Map; `language_info` defaults to
  `{"name": ""}`, `kernelspec` to `{"name": "", "display_name": ""}`)}; root array `cells`
  of Y Maps: every key of the cell as given, `id` added (uuid v4) when missing, `source` a
  Y Text (a list of strings joined), `metadata` a Y Map; code cells: `outputs` a Y Array of
  Y Maps (a `stream` output's `text` a Y Text, joined), `execution_state: "idle"`;
  markdown/raw: an empty `attachments` dropped; an empty notebook gets one empty code cell.
  Integers become floats wherever they equal their float (Yjs numbers).
- Text layout = root Y Text `source`.
- Manifest entry = `{kind: "notebook"|"text"|"file", doc?, hash?, size, by, at}`, `at`
  integer ms since epoch; notebook/text entries carry no `hash` (rule from part 2).
- Live documents: `.ipynb` → notebook; extensions the viewer shows as text
  (`workspace::TEXTS`) → text; everything else → whole file (blob).
- Sharing rules: the manifest's `rules.patterns`, else these defaults (written into a
  manifest the web app creates): `.git/`, `__pycache__/`, `.ipynb_checkpoints/`, `.venv/`,
  `build/`, `dist/`, `*.tmp`, `*~`, `.DS_Store`, `*.cdf`, `*.h5`, `*.hdf5`, `*.nc`,
  `*.fits`, `*.npy`, `*.npz`, `*.parquet`, `*.pkl`. A path a rule leaves out is skipped.
- Size cap: the server's `COCAT_FILE_MAX_MB` (app page meta `cocat-file-max`, bytes),
  applied to every file before upload; the server still enforces it on blobs.
- Uploading to a path that exists replaces it: a notebook/text path keeps its document
  (its content is replaced in one transaction, so history keeps the old version); a whole
  file gets the new hash.
- UI words: "Upload files…", "Upload a folder…", "Uploaded N files.", "Skipped: <path> —
  left out by the sharing rules" / "— too big to share (over N MB)" / "— <server's words>".
- Viewers get no upload controls. Process rules (scratchpad `p-rules.md`); gate incl. wasm
  clippy; views checked in the browser by the controller; commit on main; never push.

## Review Focus

1. A notebook uploaded from the web opens in SciQLop/JupyterLab exactly as the file: the
   Rust writer's doc equals `jupyter_ydoc`'s on the same notebook (types and values). → Task 1 fixture test.
2. Re-uploading a notebook that is open live in another tab: the other tab shows the new
   content, the file history lists the old version, restore brings it back. → Task 2 browser.
3. A folder with `.git/`, `__pycache__/` and a `.cdf` file: those are skipped and listed
   with the reason; the rest lands under the folder's name. → Task 1 tests + Task 2 browser.
4. First upload into a project without files: the file list is made once (a second tab
   uploading at the same time gets 409 and carries on). → Task 2 browser.
5. A file over the cap is refused before any byte is sent, with the cap in the message. → Task 1 test (sorting) + Task 2 browser.

---

### Task 1: Writing files in the core

**Files:** create `crates/cocat-core/src/files.rs` (+ `lib.rs` export), tests
`crates/cocat-core/tests/files.rs`, fixture `crates/cocat-core/tests/fixtures/ynotebook.bin`
with its generator `crates/cocat-core/tests/fixtures/make_ynotebook.py` (runs with
`pycrdt` + `jupyter_ydoc==4.2.1`; documents how to regenerate); move `TEXTS`' extension list
to the core if the web needs it from there (or have the core take a `is_text` predicate —
implementer's call, say which).

**Interfaces (produces):**
- `pub fn write_notebook(txn: &mut TransactionMut, notebook: &serde_json::Value) -> Result<(), FileError>` — replaces whatever the doc held.
- `pub fn write_text(txn: &mut TransactionMut, text: &str)` — replaces.
- `pub struct ManifestEntry { kind: FileKind, doc: Option<String>, hash: Option<String>, size: u64, by: Option<String>, at: i64 }`, `pub fn put_entry(txn, path: &str, entry: &ManifestEntry)`.
- `pub const DEFAULT_PATTERNS: &str`; `pub fn set_patterns(txn, text: &str)`; `pub fn patterns<T: ReadTxn>(txn: &T) -> Option<String>`.
- `pub struct Rules` built from pattern text; `fn leaves_out(&self, path: &str) -> bool` (directories in the path count, `!path` re-includes).
- `pub enum Plan { Notebook, Text, File, Skip(Skip) }`, `pub enum Skip { Rules, TooBig { cap: u64 } }`; `pub fn plan(path: &str, size: u64, rules: &Rules, cap: u64) -> Plan`.

- [ ] Failing tests: `a_notebook_is_written_as_jupyterlab_writes_it` (fixture: apply the jupyter_ydoc update to one doc, `write_notebook` the same JSON into another, compare roots recursively by Y type and value, cell ids given), `a_cell_without_an_id_gets_one`, `an_empty_notebook_gets_one_empty_code_cell`, `writing_again_replaces_the_notebook`, `a_text_file_is_its_source`, `a_manifest_entry_names_its_document_and_author`, `the_default_rules_leave_out_caches_and_data`, `a_forced_path_comes_back`, `a_file_over_the_cap_is_skipped`, `a_notebook_a_script_and_a_picture_are_sorted`.
- [ ] Implement; gate (native + wasm32 build of cocat-web must still compile with the new crate); commit.

### Task 2: Upload in the web app (controller)

**Files:** `crates/cocat-server/src/ui/app.rs` + `templates/app_head.html` (meta
`cocat-file-max`), `crates/cocat-web/src/session.rs` (`RoomConfig.file_max: Option<u64>`),
create `crates/cocat-web/src/views/upload.rs` (picker inputs, drop target, the run, the
result list), `views/files.rs` (Files section "⋯ → Upload files… / Upload a folder…", drop
on the section; expose the manifest Room to the upload), `views/sidebar.rs` ("+" menu
"Upload files for SciQLop…" when the project has no files yet), `app.rs`, `style.css`.

- [ ] Native tests: the meta tag read; the result text (`uploaded_text`, `skipped_text`) as pure functions.
- [ ] Implement: read `File`s (`gloo-file` or `web_sys::File::text/array_buffer`), plan each, create the manifest when missing (409 → re-read documents), notebook/text → `POST documents` (or reuse the entry's doc) then a socket to the document, write, wait until the server has it, close; blobs → sha256 + PUT; then all manifest entries in one transaction. A project that had no files reloads once after the first upload (`// simplify:` names it).
- [ ] Browser: upload a notebook + script + png + a folder with `.git/`, `__pycache__/`, `x.cdf`; the files show, the notebook renders; Python (`pycrdt` + `jupyter_ydoc`) reads the uploaded notebook back equal to the file; re-upload while another tab shows it live; first upload into a project without files; a viewer sees no upload controls; light + dark.
- [ ] Gate; commit.

### Task 3: Help and README (controller)

- [ ] Help (Files of a SciQLop workspace): uploading, what becomes live, the rules, the cap, replacing. README: the web upload and the `cocat-file-max` meta. Help test first; gate; commit.
