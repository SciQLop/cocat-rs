# Shared workspaces, part 2: the web viewer — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Opening a workspace in the web app shows its file tree; opening a file shows it live
(notebooks rendered, text files coloured, images inline, other files as downloads), with
each file's history and Restore for editors.

**Architecture:** The shell branches on the project's kind before it connects: a workspace
gets its own page, which syncs the manifest document and, for the opened file, that file's
document, as read replicas over `/room/{project}/{doc}` (the existing `Room` +
`connection::connect`, with the document's socket URL). What the page shows is computed by
plain native-tested modules from the documents' JSON (`workspace.rs` file tree,
`notebook.rs`, `markdown.rs`, `highlight.rs`), so live views and history views share one
renderer.

**Tech Stack:** Leptos 0.8 CSR (wasm), yrs, pulldown-cmark (markdown), syntect with
`default-fancy` (pure-Rust regex, builds for wasm32) for syntax colouring.

**Spec:** `docs/superpowers/specs/2026-11-10-workspace-sharing-design.md`, section "The web
app: a workspace viewer". Server part: `docs/superpowers/plans/2026-11-10-workspaces-server.md`
(done; routes in the README "Workspaces" section).

## Global Constraints

- Viewing only: no editing of notebook or text content in the web app.
- A workspace never connects to `/room/{project}` (the server answers 404); only to
  `/room/{project}/{doc}`.
- Manifest schema (from part 1): root map `files`, path → map
  `{kind: "notebook"|"text"|"file", doc?: <uuid>, hash?: <sha256>, size, by, at}` (`at`: integer milliseconds since the Unix epoch); root map
  `rules`, key `patterns`.
- Notebook documents use the jupyter_ydoc layout: root map `meta`, root array `cells` (maps
  with `id`, `cell_type`, `source` (text), `metadata`, `execution_count`, `outputs` (array of
  maps)); text files: root text `source`.
- Nothing a document holds may run in the page: raw HTML in markdown is shown as text, link
  and image URLs only `http:`, `https:`, `mailto:`, relative, or `data:image/(png|jpeg|gif|webp)`;
  HTML outputs render in an `<iframe sandbox>` with no `allow-scripts`; SVG only through
  `<img>` (never inlined).
- No dev words in the UI; plain short sentences (the app's existing style). Light and dark
  themes through the stylesheet's tokens.
- Process rules (scratchpad `p-rules.md`): TDD for the native modules; one build/test at a
  time; gate `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
  && cargo clippy -p cocat-web --target wasm32-unknown-unknown -- -D warnings && cargo test
  --workspace`; views are checked in the browser with desktop screenshots (phone only must
  not break); commit on main with explicit paths; never push.
- Wasm bundle: syntect must not grow `dist/*.wasm` by more than 1.5 MB gzipped; measure.

## Review Focus

1. A markdown cell with `<script>`, `<img onerror=…>`, or a `javascript:` link renders as
   harmless text/link-less text. → Task 4 tests.
2. A notebook output with only `text/html` (e.g. a pandas table) shows in a sandboxed frame,
   not as page HTML. → Task 3 test (mime choice) + Task 6 browser check.
3. A file renamed or deleted in the manifest while it is open: the page follows the
   manifest (the open view says "This file was moved or deleted" instead of showing stale
   content or crashing). → Task 6.
4. An empty workspace and a workspace whose manifest has unknown kinds or missing fields
   show sensibly (unknown entries listed as plain files, never a crash). → Task 2 tests.
5. A viewer sees no Restore button and the server refuses it anyway. → Task 7.

---

### Task 1: Workspaces in the project list, and their own page

**Files:**
- Modify: `crates/cocat-web/src/projects.rs` (`Project.kind`), `crates/cocat-web/src/app.rs`
  (Shell branches on kind), `crates/cocat-web/src/views/projects.rs` (menu: "New workspace…")
- Create: `crates/cocat-web/src/views/workspace.rs` (the `WorkspaceApp` shell: header,
  sidebar slot, main slot; empty for now)
- Test: `crates/cocat-web/src/projects.rs` tests

**Interfaces:**
- Produces: `projects::Kind { Catalogues, Workspace }` (serde from `"kind"`, default
  `Catalogues` when absent so older servers still parse); `Project.kind`; the open project's
  kind reachable from `Projects` the way `personal` is.
  `WorkspaceApp(config: RoomConfig)` component, mounted by `Shell` instead of `App` when the
  open project is a workspace; `App` is never mounted for a workspace.
  The workspace page keeps the app's header (project menu, Share with members, groups and
  invitations, Help, the account menu) exactly as a catalogue project has it; the project's
  catalogue History button is not shown (file history lives in each file, Task 7).
  "New workspace…" in the project menu creates one (`POST /api/rooms {name, kind:
  "workspace"}`) and opens it, like "New project…".

- [ ] **Step 1: Failing tests:** `a_project_without_a_kind_is_a_catalogue_project`,
  `a_workspace_is_read_as_one`.
- [ ] **Step 2:** run, see them fail.
- [ ] **Step 3:** implement; `WorkspaceApp` shows the project name and "This workspace is
  empty. Its files appear here once someone shares a SciQLop workspace into it." for now.
- [ ] **Step 4:** gate; browser: create a workspace from the menu, it opens on the empty
  page, the browser console shows no websocket to `/room/{id}` (desktop screenshot).
- [ ] **Step 5:** commit.

### Task 2: The file tree from the manifest

**Files:**
- Create: `crates/cocat-web/src/workspace.rs` (native logic), declare in `lib.rs` with the plain modules
- Test: same file

**Interfaces:**
- Produces:
  - `struct Entry { path: String, kind: FileKind, doc: Option<String>, hash: Option<String>, size: Option<u64>, by: Option<String>, at: Option<i64> }`;
  - `enum FileKind { Notebook, Text, File }` (unknown kind → `File`);
  - `fn entries(files: &serde_json::Value) -> Vec<Entry>` from the `files` root's JSON
    (entries missing a path or with a non-object value are skipped);
  - `enum Node { Folder { name: String, path: String, children: Vec<Node> }, File(Entry) }`
    and `fn tree(entries: Vec<Entry>) -> Vec<Node>`: folders first, then files, each by
    name case-insensitively;
  - `fn shows_as(entry: &Entry) -> Shown` with `enum Shown { Notebook, Text { language: Option<&'static str> }, Image, Download }`
    (by kind, then extension: `.png .jpg .jpeg .gif .webp .svg` → Image; `.py .md .txt
    .toml .json .yaml .yml .cfg .ini .csv .sh .rs .c .cpp .h .js .ts .html .css .tex` →
    Text with the matching syntect language name; anything else → Download).

- [ ] **Step 1: Failing tests:** `entries_skip_what_is_not_a_file`, `an_unknown_kind_is_a_plain_file`,
  `the_tree_lists_folders_first_by_name` (`b.txt`, `A/x.py`, `a/y.py`, `c.ipynb` → folder
  `A`, folder `a`… case-insensitive order defined in the test), `each_extension_shows_as_expected`.
- [ ] **Step 2:** run, see them fail.
- [ ] **Step 3:** implement.
- [ ] **Step 4:** gate.
- [ ] **Step 5:** commit.

### Task 3: A notebook, read for display

**Files:**
- Create: `crates/cocat-web/src/notebook.rs` (native logic)
- Test: same file

**Interfaces:**
- Produces:
  - `struct Notebook { language: String, cells: Vec<Cell> }` (`language` from
    `meta.metadata.kernelspec.language` or `language_info.name`, default `"python"`);
  - `struct Cell { id: String, kind: CellKind, source: String, count: Option<i64>, outputs: Vec<Output> }`, `enum CellKind { Markdown, Code, Raw }`;
  - `enum Output { Stream { error: bool, text: String }, Error { name: String, value: String, traceback: String }, Image { mime: String, base64: String }, Svg(String), Html(String), Text(String) }`;
  - `fn notebook(roots: &serde_json::Value) -> Notebook` from the document's roots as JSON
    (the same shape `state_at` returns and a live replica gives with `to_json` per root).
  - Mime preference for `display_data`/`execute_result`: `image/png`, `image/jpeg`,
    `image/svg+xml`, `text/html`, `text/plain`. Text fields may be a string or a list of
    strings (nbformat); both join into one string. ANSI colour codes in tracebacks and
    streams are stripped.

- [ ] **Step 1: Failing tests:** `cells_keep_their_order_and_kinds`,
  `a_plot_prefers_png_over_text`, `a_table_prefers_html_over_text`,
  `stream_text_may_be_a_list_of_lines`, `tracebacks_lose_their_colour_codes`,
  `a_notebook_without_metadata_is_python`.
- [ ] **Step 2:** run, see them fail.
- [ ] **Step 3:** implement (ANSI stripping: a small regex-free state machine is fine, or an
  established crate such as `strip-ansi-escapes` if it builds for wasm32).
- [ ] **Step 4:** gate.
- [ ] **Step 5:** commit.

### Task 4: Markdown, safely

**Files:**
- Create: `crates/cocat-web/src/markdown.rs` (native logic); add `pulldown-cmark` to cocat-web
- Test: same file

**Interfaces:**
- Produces: `fn markdown_html(text: &str) -> String` — CommonMark plus tables and
  strikethrough; every raw HTML event emitted as escaped text; link and image destinations
  kept only when allowed (Global Constraints), otherwise the link becomes plain text and the
  image its alt text.

- [ ] **Step 1: Failing tests:** `headings_lists_and_tables_render`,
  `raw_html_is_shown_as_text` (`<script>alert(1)</script>`, `<img src=x onerror=alert(1)>`),
  `a_javascript_link_loses_its_link`, `a_data_image_stays_but_data_html_does_not`.
- [ ] **Step 2:** run, see them fail.
- [ ] **Step 3:** implement.
- [ ] **Step 4:** gate.
- [ ] **Step 5:** commit.

### Task 5: Syntax colouring

**Files:**
- Create: `crates/cocat-web/src/highlight.rs` (native logic); add `syntect` (default features
  off; `default-syntaxes`, `parsing`, `regex-fancy`)
- Modify: `crates/cocat-web/style.css` (token classes, light and dark)
- Test: same file

**Interfaces:**
- Produces: `fn highlight(code: &str, language: &str) -> Vec<Vec<(Tone, String)>>` (lines
  of runs) with `enum Tone { Plain, Keyword, String, Number, Comment, Function, Type, Punctuation }`
  mapped from syntect scopes (a small table: `keyword` → Keyword, `string` → String, …);
  an unknown language returns every line as `Plain`. The view maps `Tone` to CSS classes
  (`tone-keyword`…) whose colours come from the stylesheet's tokens.

- [ ] **Step 1: Failing tests:** `python_keywords_and_strings_get_their_tones`,
  `an_unknown_language_is_plain`, `lines_are_kept`.
- [ ] **Step 2:** run, see them fail.
- [ ] **Step 3:** implement; load the syntax set once (`OnceLock`).
- [ ] **Step 4:** gate; `trunk build --release` and compare gzipped `dist/*.wasm` with the
  size before this task; over +1.5 MB → stop and report (BLOCKED) with the numbers.
- [ ] **Step 5:** commit.

### Task 6: The workspace page, live

**Files:**
- Modify: `crates/cocat-web/src/views/workspace.rs`, `crates/cocat-web/src/route.rs`
  (`Route::File(String)` ⇄ `#file/<path>`), `crates/cocat-web/style.css`
- Create: `crates/cocat-web/src/views/notebook.rs` (renders `Notebook`), and file views as
  needed inside `views/workspace.rs`
- Test: `route.rs` tests (native); browser flow

**Interfaces:**
- Consumes: Tasks 1–5.
- Produces:
  - The workspace page reads `GET /api/rooms/{id}/documents`, connects the manifest
    document (a `Room` whose `ws_url` is the project socket URL + `/{doc}`), and shows the
    tree (Task 2) in the sidebar with each file's size and "who, when" on hover.
  - `#file/<path>` opens that file: notebook/text → a second read replica of its document
    (closed when another file opens); image → `<img src>` of the blob route; download →
    a link to the blob route with the file's name as `download`.
  - Notebook view: markdown cells as `markdown_html`; code cells with `[n]` and coloured
    source; outputs: stream text (errors tinted), tracebacks, PNG/JPEG via `data:` URL,
    SVG via `<img src="data:image/svg+xml;base64,…">`, HTML in `<iframe sandbox srcdoc>`
    sized to its content where possible (fixed max height with scroll otherwise), plain text.
  - Text view: coloured lines with line numbers.
  - Live: the views read the replica through `Room::read`, so a remote edit re-renders.
  - The open file vanishing from the manifest shows "This file was moved or deleted." and
    the tree; the empty workspace keeps Task 1's sentence.

- [ ] **Step 1: Failing tests:** `a_file_route_round_trips` (paths with spaces, `/`, `#`).
- [ ] **Step 2:** run, see it fail.
- [ ] **Step 3:** implement the views.
- [ ] **Step 4:** gate; browser flow (seeded workspace with a notebook holding markdown with
  a `<script>`, a plot, a pandas-like HTML table, a traceback; a `.py` file; a `.png`; a
  `.cdf` forced file): desktop screenshots light and dark; a second client edits a cell and
  the open view changes without reload; deleting the open file's manifest entry shows the
  moved-or-deleted line.
- [ ] **Step 5:** commit.

### Task 7: A file's history and Restore

**Files:**
- Modify: `crates/cocat-web/src/views/workspace.rs` (a History tab per file),
  `crates/cocat-web/src/history.rs` only for shared request helpers it already has
- Test: native tests for any new pure helper; browser flow

**Interfaces:**
- Consumes: server `GET /api/rooms/{id}/history?document={doc}`,
  `GET /api/rooms/{id}/history/{v}/state?document={doc}` (roots as JSON),
  `POST /api/rooms/{id}/history/{v}/restore` `{document}` or, for a whole file,
  `{document: <manifest id>, path}`.
- Produces: a "History" toggle in the file view listing versions (who, when, name), picking
  one shows the file as it was with the same renderer; editors get "Restore this version"
  (with a confirm line), viewers do not; after restore the live view shows the restored
  content. Whole files: history comes from the manifest document's versions, filtered to
  versions where that path's entry changed.

- [ ] **Step 1:** failing native tests for the whole-file filter helper
  (`versions_where_a_path_changed`) if it is pure; otherwise go to browser checks.
- [ ] **Step 2:** run, see them fail.
- [ ] **Step 3:** implement.
- [ ] **Step 4:** gate; browser: edit → see two versions → view the old one → Restore as an
  editor → live view matches; as a viewer no Restore button.
- [ ] **Step 5:** commit.

### Task 8: Help

**Files:**
- Modify: `crates/cocat-server/templates/help.html` (a "Workspaces" section: what a
  workspace is, viewing files, history and restore, that editing happens in SciQLop),
  `crates/cocat-server/tests/ui_help.rs` (the new anchor)

- [ ] **Step 1:** failing help test for the `workspaces` anchor; write the section; gate; commit.
