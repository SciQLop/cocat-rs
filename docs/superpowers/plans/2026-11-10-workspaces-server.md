# Shared workspaces, part 1: the server — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** cocat-rs holds workspaces: projects of kind `workspace` with several documents (a
manifest, one per notebook or text file) synced over their own sockets, whole files as
content-addressed blobs, presence wide enough for JupyterLab, and per-file history with
restore.

**Architecture:** A document is stored and relayed exactly like today's room, under the
internal key `"{project}/{document}"` (the existing file-name escaping turns it into one flat
file in `update_dir`). The catalogues document keeps the bare project id as its key, so
nothing about catalogue projects changes. Members, roles, revocation, deletion and quota
stay the project's and fan out to every document key.

**Tech Stack:** Rust, axum, sqlx (SQLite migrations), yrs 0.28, utoipa, sha2 (already a
dependency).

**Spec:** `docs/superpowers/specs/2026-11-10-workspace-sharing-design.md`. Part 2 (web
viewer) and part 3 (SciQLop/jupyqt bridge) get their own plans once this one is merged.

## Global Constraints

- Catalogue projects and every existing client keep working unchanged: `/room/{project}` is
  the catalogues document, same file, same history.
- Project kinds: `catalogues`, `workspace`. Document kinds: `manifest`, `notebook`, `text`.
  A personal project is always `catalogues`.
- The server never reads notebook, text or manifest content except for history and restore.
- Manifest schema (fixed here, used by parts 2 and 3): root map `files`, path → map
  `{kind: "notebook"|"text"|"file", doc?: <uuid>, hash?: <sha256>, size, by, at}`; root map
  `rules`, key `patterns` → text (`.gitignore` syntax, defaults from the spec).
- Deliberately later (say so in the README): one project-wide timeline across documents
  (history here is per document); sweeping unreferenced blobs and orphaned documents
  (kept until the project is deleted).
- Whole files: per-file cap from `COCAT_FILE_MAX_MB`, default 20 MB; they count toward the
  workspace owner's existing quota (`owner_left`), no separate workspace quota.
- Viewers read everything and write nothing (sockets drop their updates, as today; blob
  upload refused).
- Process rules (scratchpad `p-rules.md`): TDD with the failing test seen first; one
  build/test at a time in the foreground; gate before each commit:
  `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings &&
  cargo clippy -p cocat-web --target wasm32-unknown-unknown -- -D warnings &&
  cargo test --workspace`; commit on main with explicit paths; never push or tag.
- Well-established crates over hand-rolled code.

## Review Focus

1. A document id from the URL that is not a uuid (`..`, `a/b`, 300 chars, empty) is refused
   (404) and never reaches the file system. → Task 3 test.
2. `/room/{catalogue-project}/{doc}` and `/room/{workspace}` (bare) answer 404; a catalogue
   client pointed at a workspace must not create a catalogues document there. → Task 3 test.
3. Removing a member or deleting a workspace closes their sockets on *every* open document
   and deletes every document file and blob. → Task 4 tests.
4. A blob whose body does not hash to the URL's sha256 is refused (400) and nothing is
   stored; the same blob uploaded twice is accepted once and counted once. → Task 6 tests.
5. A notebook's large presence (cursors, selections) still passes, but a client cannot claim
   another account in `user`. → Task 5 test.

---

### Task 1: Project kinds and documents in the database

**Files:**
- Create: `crates/cocat-server/migrations/0021_workspaces.sql`
- Modify: `crates/cocat-server/src/db.rs`
- Test: `crates/cocat-server/tests/db.rs`

**Interfaces:**
- Produces (in `db`):
  - `enum ProjectKind { Catalogues, Workspace }` with `as_str()`, sqlx-encoded as text;
  - `enum DocumentKind { Manifest, Notebook, Text }` likewise;
  - `async fn project_kind(pool, room_id: &str) -> Result<Option<ProjectKind>, DbError>`;
  - `async fn create_document(executor, room_id: &str, kind: DocumentKind, by: Uuid) -> Result<Uuid, DbError>`;
  - `async fn document_kind(pool, room_id: &str, doc: Uuid) -> Result<Option<DocumentKind>, DbError>`;
  - `async fn documents_of(pool, room_id: &str) -> Result<Vec<(Uuid, DocumentKind)>, DbError>`;
  - `create_room` gains a `kind: ProjectKind` parameter (all current callers pass `Catalogues`).

- [ ] **Step 1: Failing tests** in `tests/db.rs`:
  `a_new_project_is_a_catalogue_project_unless_asked`,
  `a_document_belongs_to_its_project` (create two documents, `documents_of` lists both with
  kinds; `document_kind` of an id from another project is `None`),
  `deleting_a_project_deletes_its_documents` (ON DELETE CASCADE).
- [ ] **Step 2:** run `cargo test -p cocat-server --test db`, see them fail to compile.
- [ ] **Step 3: Migration** `0021_workspaces.sql`:
  `room.kind TEXT NOT NULL DEFAULT 'catalogues' CHECK (kind IN ('catalogues','workspace'))`;
  `document(room_id TEXT NOT NULL REFERENCES room(id) ON DELETE CASCADE, id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('manifest','notebook','text')), created_by TEXT
  REFERENCES "user"(id) ON DELETE SET NULL, created_at DATETIME NOT NULL DEFAULT
  CURRENT_TIMESTAMP, PRIMARY KEY (room_id, id))`;
  `blob(room_id TEXT NOT NULL REFERENCES room(id) ON DELETE CASCADE, sha256 TEXT NOT NULL,
  size INTEGER NOT NULL, created_by TEXT REFERENCES "user"(id) ON DELETE SET NULL,
  created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY (room_id, sha256))`.
  Then the `db` functions above.
- [ ] **Step 4:** tests pass; full gate.
- [ ] **Step 5:** commit `migrations/0021_workspaces.sql src/db.rs tests/db.rs` (and every
  caller of `create_room` touched).

### Task 2: Creating workspaces and their documents through the API

**Files:**
- Create: `crates/cocat-server/src/documents.rs` (key helper + document API), register in `lib.rs`
- Modify: `crates/cocat-server/src/api.rs` (create/list rooms carry `kind`),
  `crates/cocat-server/src/lifecycle.rs` (`create_room` takes a kind; a workspace gets its
  manifest document in the same transaction), `crates/cocat-server/src/openapi.rs`
- Test: `crates/cocat-server/tests/workspaces.rs`

**Interfaces:**
- Consumes: Task 1.
- Produces:
  - `documents::key(room_id: &str, doc: Uuid) -> String` = `format!("{room_id}/{doc}")`;
  - `documents::MANIFEST`: the manifest's id is the one created with the workspace, listed
    with kind `manifest`;
  - `POST /api/rooms` body `{name, kind?: "catalogues"|"workspace"}` (default catalogues);
    room JSON gains `"kind"`;
  - `GET /api/rooms/{id}/documents` → `[{id, kind, created_by, created_at}]` (any member);
  - `POST /api/rooms/{id}/documents` body `{kind: "notebook"|"text"}` → 201 `{id, kind}`
    (editors and owner; 403 viewer; 409 on a catalogue project; 400 for `manifest`).

- [ ] **Step 1: Failing tests** in `tests/workspaces.rs` (build the app as `tests/api.rs`
  does): `a_workspace_starts_with_its_manifest`, `an_editor_adds_a_notebook`,
  `a_viewer_cannot_add_a_document` (403), `a_catalogue_project_holds_no_documents` (409),
  `nobody_adds_a_second_manifest` (400), `the_project_list_says_each_kind`.
- [ ] **Step 2:** run, see them fail.
- [ ] **Step 3:** implement; refusals reuse `ApiError` and the neighbouring wording style.
- [ ] **Step 4:** tests pass; gate.
- [ ] **Step 5:** commit.

### Task 3: A socket per document

**Files:**
- Modify: `crates/cocat-server/src/ws.rs` (route `/room/{room_id}/{doc_id}`; bare route
  refuses workspaces), `crates/cocat-server/src/documents.rs`
- Test: `crates/cocat-server/tests/workspaces.rs` (socket tests, helpers as in `tests/websocket.rs`)

**Interfaces:**
- Consumes: `documents::key`, `db::document_kind`, `db::project_kind`.
- Produces: `/room/{room_id}/{doc_id}` admits members of the project (same `lobby_role`,
  same subprotocol, same role header) and relays the document stored under
  `documents::key(room_id, doc_id)`. `mark_seen` keeps using the project id.

- [ ] **Step 1: Failing tests:** `two_members_co_edit_a_notebook` (A writes to a Y map,
  B receives it; the file `update_dir/<escaped key>.y` exists),
  `a_viewer_reads_a_notebook_but_never_writes_it`,
  `a_stranger_is_refused_a_notebook` (403),
  `a_document_id_that_is_not_a_uuid_is_unknown` (`..`, `a%2Fb`, 300 chars → 404, no file
  created), `an_unknown_document_is_refused` (random uuid → 404),
  `a_catalogue_project_has_no_document_sockets` (404),
  `a_workspace_has_no_catalogues_socket` (bare `/room/{workspace}` → 404).
- [ ] **Step 2:** run, see them fail.
- [ ] **Step 3:** parse `doc_id` as `Uuid` first (anything else 404); check the document
  belongs to the project before upgrading; pass the key where `bridge` takes the room id
  today, and keep the project id for roles and presence (`Presence::for_room` gets the key's
  project). Bare route: 404 when `project_kind` is `Workspace`.
- [ ] **Step 4:** tests pass, and `tests/websocket.rs` unchanged and green; gate.
- [ ] **Step 5:** commit.

### Task 4: What acts on a project acts on all its documents

**Files:**
- Modify: `crates/cocat-server/src/room.rs` (`revoke`, `delete`, `owner_left`),
  `crates/cocat-server/src/lifecycle.rs` / `api.rs` / `groups.rs` wherever they call
  `rooms.revoke` or `rooms.delete` with a project id
- Test: `crates/cocat-server/tests/workspaces.rs`, `crates/cocat-server/tests/quotas.rs`

**Interfaces:**
- Produces on `RoomManager`:
  - `async fn revoke_project(&self, pool: &SqlitePool, room_id: &str, user: Uuid, reason: CloseReason)`;
  - `async fn delete_project(&self, pool: &SqlitePool, room_id: &str)` (documents' files,
    stamps, and the project's blob folder from Task 6);
  - `owner_left` counts, for each owned project, the catalogues key plus every document key
    plus the project's stored blob bytes.
  Every existing caller of `revoke`/`delete` with a project id moves to the `_project`
  versions; the single-key versions stay for internal use.

- [ ] **Step 1: Failing tests:** `removing_a_member_closes_their_notebook_sockets`,
  `lowering_a_group_grant_closes_notebook_sockets` (via groups, as `tests/groups_ws.rs`),
  `deleting_a_workspace_deletes_every_document_file`,
  `a_workspace_documents_count_toward_the_owner_quota` (in `tests/quotas.rs`: a notebook
  write that pushes past `COCAT_OWNER_MAX_MB` is refused like a catalogue write).
- [ ] **Step 2:** run, see them fail.
- [ ] **Step 3:** implement; the document keys come from `db::documents_of`.
- [ ] **Step 4:** tests pass; gate.
- [ ] **Step 5:** commit.

### Task 5: Presence for notebooks and text files

**Files:**
- Modify: `crates/cocat-server/src/presence.rs`, the `Presence::for_room` call in `room.rs`
- Test: unit tests in `presence.rs`; `crates/cocat-server/tests/awareness.rs`

**Interfaces:**
- Produces: `Presence::for_document(room_id: &str, kind: Option<DocumentKind>)`;
  `None` (catalogues) behaves exactly as `for_room` today. For `Notebook`/`Text`/`Manifest`:
  every field of the client's state is kept except `user`, which is replaced by the
  authenticated account (`{email, color}` as today, plus `name` and `display_name` set to
  the email's part before `@` so JupyterLab shows a name); frame cap
  `MAX_DOCUMENT_FRAME = 16 * 1024` instead of 4096.

- [ ] **Step 1: Failing tests:** `a_notebook_keeps_cursors_and_selections`,
  `a_notebook_state_cannot_claim_another_account` (state with `user.email` of someone else →
  relayed with the socket's own account), `a_notebook_frame_over_16_kb_is_dropped`,
  `a_catalogue_still_keeps_only_user_and_editing` (unchanged behaviour).
- [ ] **Step 2:** run, see them fail.
- [ ] **Step 3:** implement as a policy chosen once per room thread from the key's document
  kind (looked up when the room opens).
- [ ] **Step 4:** tests pass; gate.
- [ ] **Step 5:** commit.

### Task 6: Whole files as blobs

**Files:**
- Create: `crates/cocat-server/src/blobs.rs` (routes + storage), register in `lib.rs`/`openapi.rs`
- Modify: `crates/cocat-server/src/cli.rs` (`COCAT_FILE_MAX_MB`, default 20, same parser as
  `COCAT_ROOM_MAX_MB`), `crates/cocat-server/src/room.rs` (`StorageLimits.file`)
- Test: `crates/cocat-server/tests/blobs.rs`

**Interfaces:**
- Produces:
  - `PUT /api/rooms/{id}/blobs/{sha256}` raw body → 201 (new) or 200 (already stored);
    400 hash mismatch or malformed sha (64 lowercase hex); 403 viewer/stranger; 409 on a
    catalogue project; 413 over `COCAT_FILE_MAX_MB`; the owner-quota refusal status and
    wording the room writes use today when `owner_left` is short.
  - `GET /api/rooms/{id}/blobs/{sha256}` → bytes (`application/octet-stream`), any member;
    404 when not stored for that project.
  - Storage: `update_dir/blobs/<escaped room id>/<sha256>`, written to a temp file then
    renamed; a row in `blob`.
  - `blobs::stored_bytes(pool, room_id) -> u64` for Task 4's quota.
  - `// simplify:` blobs are kept until the project is deleted (no sweep). Ceiling: a busy
    workspace keeps every version it ever uploaded, bounded by the owner quota. Upgrade:
    sweep blobs no manifest entry and no history version within retention refers to (spec).

- [ ] **Step 1: Failing tests:** `an_editor_stores_and_a_viewer_reads_a_file`,
  `a_body_that_does_not_match_its_hash_is_refused` (400, no file, no row),
  `the_same_file_twice_is_stored_once` (second PUT 200, `stored_bytes` unchanged),
  `a_file_over_the_cap_is_refused` (413 with `COCAT_FILE_MAX_MB=1`),
  `a_file_past_the_owner_quota_is_refused`, `a_viewer_cannot_store_a_file` (403),
  `deleting_the_workspace_deletes_its_files`.
- [ ] **Step 2:** run, see them fail.
- [ ] **Step 3:** implement; hash with `sha2::Sha256` while streaming the body with the cap
  enforced by axum's body limit for this route.
- [ ] **Step 4:** tests pass; gate.
- [ ] **Step 5:** commit.

### Task 7: History and restore for one file

**Files:**
- Create: `crates/cocat-core/src/restore.rs` (generic restore), export from `lib.rs`
- Modify: `crates/cocat-server/src/history_api.rs` (an optional `document` query/body on
  `list`, `state_at` and `restore`)
- Test: `crates/cocat-core/tests/restore.rs`, `crates/cocat-server/tests/history_api.rs`

**Interfaces:**
- Produces:
  - `cocat_core::restore::restore_document(txn: &mut TransactionMut, past: &Doc, only: Option<&str>)`:
    makes every root type of the live document equal to `past`'s, as a minimal set of
    edits (maps: set changed keys, remove missing ones, recurse into nested maps; arrays:
    rebuild; text: one diff, using `similar` for the text diff). `only` limits a
    manifest restore to one root-map key (`files` entry = one path).
  - `GET /api/rooms/{id}/history?document={doc}` lists that document's versions (stamps are
    per key already); `POST .../history/{version}/restore` body `{document?, path?}`
    restores that document (or one manifest path) through `write_as` as the caller.
    Without `document`, behaviour is today's (catalogues).
  - Catalogue-specific "what changed" (`changes`) stays catalogues-only; on a document it
    answers 409 "Changes are listed for catalogues only".

- [ ] **Step 1: Failing tests:** core: `restoring_brings_back_deleted_cells_and_text`
  (a YNotebook-shaped doc: `cells` array of maps with `source` text; edit, delete a cell,
  restore → equal JSON to the past), `restoring_one_manifest_path_leaves_the_others`.
  Server: `a_notebook_has_its_own_history`, `an_editor_restores_a_notebook`,
  `a_viewer_cannot_restore`, `catalogue_history_is_unchanged` (existing tests stay green).
- [ ] **Step 2:** run, see them fail.
- [ ] **Step 3:** implement (`similar` is the established text-diff crate; add it).
- [ ] **Step 4:** tests pass; gate.
- [ ] **Step 5:** commit.

### Task 8: Document it

**Files:**
- Modify: `README.md` (a "Workspaces" section: kinds, document and blob routes, sockets,
  `COCAT_FILE_MAX_MB`, who pays), `docs/deploy.md` (the new variable)

- [ ] **Step 1:** write the section; check every route and variable named exists
  (`curl` the OpenAPI JSON from a test server or read `openapi.rs`).
- [ ] **Step 2:** gate; commit.
