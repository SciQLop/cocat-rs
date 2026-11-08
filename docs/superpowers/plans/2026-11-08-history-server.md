# Room history on the server — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The server records who wrote each room update and when, and serves a Google-Docs-style
version history: the version list, the diff of a version, the room at a version, named
versions, and restore.

**Architecture:** Every record the room appends gets a stamp (record index, user, time, length,
xxh3 hash) in a side file `<room file>.h`. History is computed on request by replaying the
room's records (`cocat_core::store::read_updates`) with deep observers that collect what each
record touched, then grouping records into versions. Restore builds an update on the room's
own thread, so it is stamped and broadcast like any client write.

**Tech Stack:** Rust, yrs 0.28, axum + utoipa (existing `/api`), sqlx SQLite migrations,
`xxhash-rust` (feature `xxh3`), PyO3 for the Python surface.

**Spec:** `docs/superpowers/specs/2026-11-08-web-catalogue-browser-design.md` (goals 5–6,
"Decided", "Settled after the builder check" 1–3 and 6). The browser UI is a separate plan.

## Global Constraints

- TDD: the failing test first, then the code. Full gate before each commit:
  `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
  and, when cocat-py changes, the Python suite (see README "Tests").
- One build or test command at a time, in the foreground. Never two cargo runs in parallel.
- Commit straight to `main`; never push. `git grep -n -I -E "/home/jeandet|/tmp/claude-"` stays empty.
- History never rewrites the room file: restore and naming only append or touch the database.
- A version's author is the account that sent its records; records without a valid stamp are
  "before history" (author `None`), still listed and diffable.
- Versions group consecutive records by the same author with no gap over 5 minutes
  (`VERSION_GAP_MS = 300_000`); consecutive unstamped records form one version.
- Restore moves events and catalogues created after the version to the trash (status
  `deleted`), never purges.
- Read routes need room membership (any role); naming and restore need a role that can write
  (`Role::can_write`). Non-members get the same 404 as an unknown room (existing `membership`).

## Review Focus

- A crash between appending a record and its stamp, or a stamp file longer than the room
  file: stamps without a matching record are ignored, the room still loads.
- A re-seeded room file (dev copies production over the `.y`): stamps whose length or hash no
  longer match are dropped, those records read "before history", nothing errors.
- Restoring a version whose events were since purged: the events come back whole from the past
  state, not as empty maps.
- A viewer calling restore or naming: refused (403 per the existing API's role errors), and the
  room is unchanged.
- Deleting a room removes its stamps file too; a later room with the same id starts clean.

---

### Task 1: Stamps are written beside every appended record

**Files:**
- Create: `crates/cocat-server/src/history/mod.rs`, `crates/cocat-server/src/history/stamps.rs`
- Modify: `crates/cocat-server/src/lib.rs` (add `pub mod history;`), `crates/cocat-server/Cargo.toml`
  (`xxhash-rust = { version = "0.8", features = ["xxh3"] }`), `crates/cocat-core/src/store.rs`
  (`FileRoomStore::record_count`), `crates/cocat-server/src/room.rs` (`handle_update` takes the
  writer; stamps written and synced with the store; room deletion removes the stamps file)
- Test: `crates/cocat-server/tests/history_stamps.rs`, unit tests in `stamps.rs`

**Interfaces:**
- Produces:
  - `pub struct Stamp { pub record: u64, pub user: Option<Uuid>, pub at_ms: i64, pub len: u32, pub hash: u64 }`
  - `pub fn stamp_for(record: u64, user: Option<Uuid>, at_ms: i64, bytes: &[u8]) -> Stamp` (xxh3_64 of the bytes)
  - `pub fn stamps_file(update_dir: &Path, room_id: &str) -> PathBuf` = `room_file(...)` with `.h` appended (`x.y.h`)
  - `pub struct StampFile` with `open(update_dir, room_id) -> io::Result<StampFile>`, `append(&mut self, Stamp)`,
    `write(&mut self) -> io::Result<()>`, `sync(&mut self) -> io::Result<()>` mirroring `FileRoomStore`'s cadence
  - `pub fn read_stamps(update_dir: &Path, room_id: &str) -> io::Result<Vec<Stamp>>` (missing file = empty; a torn
    last entry is ignored)
  - `pub fn valid_stamps(records: &[Bytes], stamps: Vec<Stamp>) -> Vec<Option<Stamp>>` — one slot per record,
    `Some` only when the stamp's record exists and its len and hash match
  - `FileRoomStore::record_count(&self) -> u64` (records loaded plus appended)
- Entry encoding: fixed 48 bytes little-endian: record u64, user 16 bytes (all zero = none), at_ms i64, len u32,
  4 zero bytes, hash u64.

- [ ] **Step 1: Write the failing tests**
  - `stamps.rs` units: `a_stamp_round_trips_through_the_file` (two stamps, one with user None, read back equal);
    `a_torn_last_entry_is_ignored` (append 20 garbage bytes → one stamp read);
    `stamps_that_do_not_match_their_record_are_dropped` (records `[a, b, c]`, stamps for 0 and 2 where 2's hash is
    for other bytes, plus one for record 7 → `[Some, None, None]`).
  - `tests/history_stamps.rs` (through a real server, like `tests/websocket.rs`):
    `an_update_from_a_client_is_stamped_with_its_user` — client writes one update, room flushed, `read_stamps`
    gives one stamp whose user is the client's account and whose hash validates against `read_updates`;
    `a_room_file_swapped_underneath_drops_its_stamps` — after the write, replace the `.y` with a different valid
    file, `valid_stamps` yields all `None`;
    `deleting_the_room_removes_its_stamps`.
  - `crates/cocat-core` store unit: `room_id_from_file` returns `None` for `"lab.y.h"`.
- [ ] **Step 2: Run them, expect failures** (`cargo test -p cocat-server --test history_stamps`, `cargo test -p cocat-server history::`).
- [ ] **Step 3: Implement.** `handle_update(doc, store, stamps, seen, limit, payload, user: Option<Uuid>)`:
  after a successful `store.append`, append `stamp_for(store.record_count() - 1, user, now_ms, &payload)`.
  The frame path passes `Some(client.user)`; the unit tests in `room.rs` pass `None`. Write and sync the
  stamps wherever the store is written and synced. A stamps file that cannot be opened (e.g. the name passes 255
  bytes) logs one warning and the room runs without stamps. `cocat-server import`'s migration append stays
  unstamped.
- [ ] **Step 4: Run the tests, expect PASS; run the full gate.**
- [ ] **Step 5: Commit** "Stamp every room record with its writer and time".

### Task 2: The version list

**Files:**
- Create: `crates/cocat-server/src/history/versions.rs`
- Test: unit tests in `versions.rs` (records built with `cocat_core::model` writes on a scratch `Doc`,
  each transaction encoded as one record)

**Interfaces:**
- Consumes: `Stamp`, `valid_stamps` (Task 1).
- Produces:
  - `pub const VERSION_GAP_MS: i64 = 300_000;`
  - `#[derive(Serialize, utoipa::ToSchema)] pub struct Version { pub id: u64 /* last record */, pub first: u64,
    pub author: Option<Uuid>, pub started_ms: Option<i64>, pub ended_ms: Option<i64>, pub via: Option<String>,
    pub auto_name: Option<String>, pub events_added: u32, pub events_changed: u32, pub events_removed: u32,
    pub catalogues: Vec<String>, pub rules_changed: Vec<String> }`
  - `pub struct Touched { events_added: BTreeSet<String>, events_changed: BTreeSet<String>,
    events_removed: BTreeSet<String>, catalogues: BTreeSet<String>, rules: BTreeSet<String>, origins_added: Vec<String> }`
  - `pub fn touched_by_records(records: &[Bytes]) -> Vec<Touched>` — replays into one `Doc` with `observe_deep`
    on the events, catalogues and origins roots; one `Touched` per record.
  - `pub fn versions(records: &[Bytes], stamps: &[Option<Stamp>]) -> Vec<Version>` — newest first.
- Rules: an event key appearing in the events root = added; leaving it, or its `status` becoming `deleted` =
  removed; any other key change inside an event = changed (an event both added and changed in one version counts
  as added). A catalogue is touched when its own map, its `events` set or its `rule` changes; `rules_changed` lists
  catalogues whose `rule` key changed. `via`: the name of the origin that the version's new events reference, or
  of an origin the version added. `auto_name`: `"{kind}: {name}"` when the version added an origin of kind
  `import` or `rule`.

- [ ] **Step 1: Write the failing tests:** `records_by_one_author_within_five_minutes_make_one_version`;
  `a_gap_over_five_minutes_starts_a_new_version`; `another_author_starts_a_new_version`;
  `unstamped_records_group_into_one_version_before_history` (author None, times None);
  `counts_added_changed_and_removed_events` (create 2 events, edit one, trash a third pre-existing →
  2/0/1 in that version, then 0/1/0 in the next); `a_rule_edit_is_named_in_rules_changed`;
  `an_import_origin_names_the_version_and_its_events_carry_via`; `versions_are_newest_first`.
- [ ] **Step 2: Run, expect failures.**
- [ ] **Step 3: Implement** `touched_by_records` and `versions`.
- [ ] **Step 4: Run, expect PASS; full gate.**
- [ ] **Step 5: Commit** "Group stamped records into versions".

### Task 3: The diff of a version

**Files:**
- Create: `crates/cocat-server/src/history/diff.rs`
- Test: unit tests in `diff.rs`

**Interfaces:**
- Produces:
  - `pub fn replay_until(records: &[Bytes], end: u64) -> Doc` (records `0..=end`; `end = u64::MAX` none skipped;
    a version `first == 0` diffs against an empty `Doc`)
  - `#[derive(Serialize, utoipa::ToSchema)] pub enum Change { Added, Changed, Removed }` (serialised lowercase)
  - `pub struct EventChange { pub uuid: String, pub change: Change, pub fields: Vec<String>,
    pub before: Option<EventRecord>, pub after: Option<EventRecord> }`
  - `pub struct CatalogueChange { pub uuid: String, pub name: String, pub change: Change, pub fields: Vec<String> }`
    (`fields` includes `"events"` when members changed, `"rule"` when the rule changed)
  - `pub struct Diff { pub events: Vec<EventChange>, pub catalogues: Vec<CatalogueChange> }`
  - `pub fn diff(before: &Doc, after: &Doc, catalogue: Option<&str>) -> Diff` — compares `model` reads; with a
    catalogue, only events that are its members on either side (smart catalogues: members per
    `model::catalogue_events`) and only that catalogue.
- An event whose status became `deleted` is `Removed`; one that left the root is `Removed` with `after: None`.

- [ ] **Step 1: Failing tests:** `an_added_event_has_no_before`; `a_changed_event_lists_only_its_changed_fields`
  (confidence and status changed → `fields == ["confidence", "status"]`); `a_trashed_event_is_removed`;
  `a_catalogue_filter_keeps_only_its_members`; `a_member_added_to_a_catalogue_shows_events_in_its_fields`.
- [ ] **Step 2–4:** run (fail), implement, run (pass), full gate.
- [ ] **Step 5: Commit** "Diff two states of a room".

### Task 4: History routes and named versions

**Files:**
- Create: `crates/cocat-server/src/history_api.rs`, `crates/cocat-server/migrations/0008_version_names.sql`
- Modify: `crates/cocat-server/src/api.rs` or the router assembly in `lib.rs` (merge `history_api::router()` the
  way `catalogues_api` is merged), `crates/cocat-server/src/db.rs` (name queries)
- Test: `crates/cocat-server/tests/history_api.rs`, and the route rows in `tests/access_matrix.rs`

**Interfaces:**
- Consumes: Tasks 1–3. The room is flushed (`RoomHandle::flush` when open) before records are read.
- Migration: `CREATE TABLE version_name (room_id TEXT NOT NULL REFERENCES room(id) ON DELETE CASCADE,
  version INTEGER NOT NULL, name TEXT NOT NULL, created_by TEXT REFERENCES "user"(id) ON DELETE SET NULL,
  created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY (room_id, version));`
- Routes (all documented with `utoipa::path`, cookie security, `Detail` errors like `catalogues_api`):
  - `GET /api/rooms/{room_id}/history?catalogue=<uuid>` → `{ "versions": [VersionView] }` where `VersionView` is
    `Version` plus `author_email: Option<String>` and `name: Option<String>` (stored name, else `auto_name`);
    with `catalogue`, only versions whose `catalogues` contain it, or (smart catalogue) that changed any event.
  - `GET /api/rooms/{room_id}/history/{version}?catalogue=<uuid>` → `Diff` between the state before the
    version's first record and after its last.
  - `GET /api/rooms/{room_id}/history/{version}/state` → `application/octet-stream`: one v1 update holding the
    room as of that version (`encode_state_as_update_v1` of `replay_until`).
  - `PUT /api/rooms/{room_id}/history/{version}/name` body `{ "name": "..." }` (1–100 chars after trim) → 204;
    `DELETE` same path → 204.
  - An unknown version id → 404 `{"detail": "No such version"}`.
- `simplify:` the list replays the whole log per request (0.7 s on the largest production room); cache the
  `Vec<Version>` per room keyed by record count when that hurts.

- [ ] **Step 1: Failing tests:** `members_list_versions_with_author_emails`; `a_catalogue_filter_narrows_the_list`;
  `the_diff_of_a_version_names_its_changes`; `the_state_of_a_version_replays_to_that_point` (apply the returned
  bytes to a fresh `Doc`, the later event is absent); `an_editor_names_a_version_and_the_list_shows_it`;
  `a_viewer_cannot_name_a_version`; `strangers_get_404`; `an_unknown_version_is_404`; access-matrix rows.
- [ ] **Step 2–4:** run (fail), implement, run (pass), full gate.
- [ ] **Step 5: Commit** "History routes: versions, diffs, past states and names".

### Task 5: Restore

**Files:**
- Create: `crates/cocat-core/src/model/restore.rs` (exported from `model`)
- Modify: `crates/cocat-server/src/room.rs` (`RoomCommand::Write` and `RoomHandle::write_as`),
  `crates/cocat-server/src/history_api.rs` (route)
- Test: unit tests in `restore.rs`; `crates/cocat-server/tests/history_api.rs`

**Interfaces:**
- Produces:
  - `pub fn restore(txn: &mut TransactionMut, past: &Doc, catalogue: Option<&str>)` in `cocat_core::model`:
    makes the events and catalogues roots (or, with a catalogue, that catalogue and its past and present member
    events) equal to `past`: keys copied or removed so each map equals its past self, nested set maps (a
    catalogue's `events`) included; events and catalogues absent from `past` get `status = "deleted"`.
    `origins`, `comments` and `attributes` roots are left alone.
  - `RoomHandle::write_as(&self, user: Uuid, edit: Box<dyn FnOnce(&Doc) -> Option<Vec<u8>> + Send>) -> Result<bool, RoomError>`:
    runs on the room thread; a returned update goes through `handle_update` with `Some(user)` (quota, append,
    stamp) and is broadcast to every client; `Ok(false)` when nothing changed.
  - `POST /api/rooms/{room_id}/history/{version}/restore` body `{ "catalogue": "<uuid>" | null }` → 200
    `{ "restored": true|false }`; needs a writing role.
- The edit closure replays the past state (`replay_until`) off the room thread before sending the command, and
  inside the room thread runs `restore` in one transaction and returns its v1 update.

- [ ] **Step 1: Failing tests:** in `restore.rs`: `restoring_brings_back_changed_fields`;
  `events_created_since_go_to_the_trash`; `a_purged_event_comes_back_whole`;
  `a_catalogue_restore_leaves_other_catalogues_alone`; `restoring_the_present_changes_nothing` (no update
  produced). In `history_api.rs`: `restore_is_broadcast_to_a_connected_client_and_stamped_with_the_restorer`;
  `a_viewer_cannot_restore`; `a_restore_is_itself_a_version_and_can_be_undone`.
- [ ] **Step 2–4:** run (fail), implement, run (pass), full gate.
- [ ] **Step 5: Commit** "Restore a room or a catalogue to a past version".

### Task 6: History from Python

**Files:**
- Modify: `crates/cocat-client/src/login.rs` (or a new `history.rs` beside it), `crates/cocat-py/src/lib.rs`,
  `crates/cocat-py/python/cocat_rs/__init__.py`, `crates/cocat-py/python/cocat_rs/_native.pyi`, `README.md`
  (Python section: a "History" paragraph)
- Test: `crates/cocat-client/tests/` (against a real server, like the existing login tests),
  `crates/cocat-py/tests/test_history.py`

**Interfaces:**
- Consumes: Task 4 routes, Task 5 restore route.
- Produces (Python):
  - `cocat_rs.history(url: str, cookie: str, room_id: str, catalogue: str | None = None) -> list[dict]` (the
    `VersionView` JSON)
  - `cocat_rs.Room.at(url: str, cookie: str, room_id: str, version: int) -> Room` — a read-only `Room` built from
    the version's state bytes, not connected; writing to it raises `RuntimeError("a past version is read-only")`.
  - `cocat_rs.restore(url: str, cookie: str, room_id: str, version: int, catalogue: str | None = None) -> bool`
  - `cocat_rs.name_version(url: str, cookie: str, room_id: str, version: int, name: str | None) -> None`
    (`None` removes the name)

- [ ] **Step 1: Failing tests** (`test_history.py`, with the existing server fixture):
  `history_lists_a_version_by_the_writer`; `room_at_a_version_shows_the_past`; `a_past_room_is_read_only`;
  `restore_from_python_undoes_an_edit`; `a_named_version_comes_back_named`.
- [ ] **Step 2–4:** run (fail), implement, run (pass), full gate including the Python suite.
- [ ] **Step 5: Commit** "History from Python: list, past rooms, restore, names".
