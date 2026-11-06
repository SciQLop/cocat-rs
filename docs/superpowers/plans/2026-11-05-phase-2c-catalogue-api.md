# Phase 2c — Read-only catalogue API — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let scripts and web pages read a room's catalogues and events over plain HTTP
(JSON and VOTable), without a CRDT client and without pinning rooms in memory.

**Architecture:** `cocat-core` learns to *read* the cocat document layout (`model` module)
and to write a VOTable (`votable` module); both are pure functions over a `yrs` read
transaction. `cocat-server` gets `RoomManager::read`, which runs a closure on the room
thread when the room is open, and otherwise loads the `.y` into a short-lived `Doc` on a
blocking thread (never registered, file never truncated). A `catalogues_api` module holds
the four routes.

**Tech Stack:** existing crates; new in `cocat-core`: `time` 0.3 (already in the lock
file; features `parsing`, `formatting`, `macros`), `serde` + `serde_json`, `uuid`.
No XML crate: the VOTable is a fixed template plus escaping.

**Spec:** `docs/superpowers/specs/2026-11-05-cocat-rs-design.md`, section "Read-only
catalogue API (new)". Behaviour reference for the document layout and the VOTable:
cocat-python-tests `rust-spike`, `src/cocat/{models.py,event.py,catalogue.py,votable.py}`.

## Global Constraints

- Worktree `../cocat-rs-p1`, branch `phase-1`. TDD, rules file,
  and the full gate after the *last* edit of each task:
  `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --workspace`.
  One build or test command at a time, in the foreground, never backgrounded.
- Never push. Commit per task.
- Document layout (from Python cocat, must be read exactly): root maps `events` and
  `catalogues`, keyed by uuid string. Event map: `uuid`, `start`, `stop` (strings),
  `author`, `tags` and `products` (maps used as sets, values `true`), `rating` (int or
  null), `attributes` (map). Catalogue map: `uuid`, `name`, `author`, `tags`, `events`
  (map-as-set of event uuids), `attributes`.
- Stored times are Python `str(datetime)`: `YYYY-MM-DD HH:MM:SS[.ffffff][±HH:MM]`.
  Also accept `T` as separator and a trailing `Z`. A time without an offset is UTC
  (cocat and SciQLop work in UTC). Times are always *output* as
  `YYYY-MM-DDTHH:MM:SS.ffffffZ` (six digits, UTC).
- All routes are under `/api`, need the session cookie (401 otherwise), accept any member
  role including viewer, and answer non-members, unknown rooms and unknown catalogues
  with 404 `{"detail": ...}` (same body shape as every other route).
- REST reads never open a room in the registry and never write to a room file.

## Review Focus

1. **A malformed event** (a time that doesn't parse, a missing field, a rating that is a
   string), written by a client that isn't cocat. Expect: that event is left out of the
   answer and logged at `warn` with the room and uuid; the request still answers 200.
   → test in Task 1.
2. **A catalogue that lists a deleted event** (dangling uuid in its `events` set; Python
   raises `KeyError` there). Expect: skipped; `event_count` counts only events that
   exist. → test in Task 1.
3. **Paging over events with equal start times**, and a tampered cursor. Expect: the
   order is (start, uuid), so no event is repeated or lost across pages; a cursor that
   doesn't decode is 422. → tests in Task 4.
4. **A room that unloads while a REST read is queued on it.** Expect: the read falls back
   to the file and answers; it never hangs. → test in Task 3.
5. **A cold read while the room thread is appending** (open between the check and the
   load). Expect: the reader stops at the last complete record, never truncates the file,
   and the room thread's next append is intact. → test in Task 3.

---

### Task 1: Read the document layout (`cocat-core::model`)

**Files:**
- Create: `crates/cocat-core/src/model.rs`, `crates/cocat-core/src/time.rs`
- Create: `crates/cocat-core/tests/fixtures/catalogues.y` (written by pycrdt, see Step 1)
- Modify: `crates/cocat-core/src/lib.rs`, `crates/cocat-core/Cargo.toml`
- Test: `crates/cocat-core/tests/model.rs`

**Interfaces:**
- Produces:
  - `pub struct Timestamp(time::OffsetDateTime)` in `time.rs`, always UTC, with
    `pub fn parse(text: &str) -> Option<Timestamp>`, `Display` in the output format above,
    `Ord`, `Serialize` (as its `Display` string) and `pub fn unix_micros(self) -> i128`.
  - `#[derive(Debug, Clone, PartialEq, Serialize)] pub struct CatalogueSummary { uuid: String,
    name: String, author: String, tags: Vec<String>, attributes: serde_json::Map<String,
    serde_json::Value>, event_count: usize }` (fields `pub`).
  - `#[derive(Debug, Clone, PartialEq, Serialize)] pub struct EventRecord { uuid: String,
    start: Timestamp, stop: Timestamp, author: String, tags: Vec<String>, products:
    Vec<String>, rating: Option<i64>, attributes: serde_json::Map<String, Value> }`.
  - `pub fn catalogues<T: ReadTxn>(txn: &T) -> Vec<CatalogueSummary>` sorted by
    (name, uuid).
  - `pub fn catalogue<T: ReadTxn>(txn: &T, uuid: &str) -> Option<CatalogueSummary>`.
  - `pub fn catalogue_events<T: ReadTxn>(txn: &T, uuid: &str) -> Option<Vec<EventRecord>>`
    sorted by (start, uuid); `None` when the catalogue doesn't exist.
  - Sets (`tags`, `products`) come out as sorted `Vec<String>`.

- [ ] **Step 1: Write the fixture.** A small script in `crates/cocat-core/tests/fixtures/
  make_catalogues.py` (committed, run once with cocat-python-tests's venv) builds through `cocat.DB`:
  catalogue "alpha" (author "a@x.org", tags {"t1","t2"}, attributes {"mission": "MMS",
  "level": 2}) with three events, one of them with microseconds and one with an explicit
  `+00:00` offset, products {"p1"}, rating 3, attributes {"q": 1.5, "flag": true}; catalogue
  "beta" with no events; then adds a dangling uuid to beta's `events` map directly through
  pycrdt, and one raw event map whose `start` is `"not a time"`. It writes the updates as a
  `.y` file with the existing record format (`"0.0.1\0"` header, `varuint(len)+update`),
  and prints the expected JSON (from `cocat.DB`'s own view) to
  `fixtures/catalogues.expected.json`.

- [ ] **Step 2: Write the failing tests** in `tests/model.rs`, loading the fixture with the
  existing `FileRoomStore::open` into a `Doc`:
  - `lists_catalogues_sorted_by_name`: names `["alpha", "beta"]`, alpha's `event_count == 3`,
    tags `["t1","t2"]`, attributes `{"level": 2, "mission": "MMS"}`.
  - `dangling_event_is_not_counted`: beta's `event_count == 0`, `catalogue_events` is
    `Some(vec![])`.
  - `events_match_python`: alpha's events equal `catalogues.expected.json`, field by field.
  - `malformed_event_is_skipped`: the raw event with `"not a time"` appears nowhere and
    nothing panics.
  - `unknown_catalogue_is_none`.
  - In `time.rs` unit tests: `"2025-01-31 00:00:00"`, `"2025-01-31 00:00:00+00:00"`,
    `"2025-01-31T01:02:03.004500Z"`, `"2025-01-31 02:00:00+02:00"` all parse, and the last
    prints `2025-01-31T00:00:00.000000Z`; `"2025-13-01 00:00:00"` and `""` don't parse.

- [ ] **Step 3: Run them, expect FAIL** (modules missing): `cargo test -p cocat-core`.

- [ ] **Step 4: Implement.** Read nested values with `Out::to_json(&txn)` and then
  `serde_json::to_value` on the resulting `Any`, so plain values and nested maps read
  the same way. A rating may arrive as an integer or as a float with no fractional part;
  anything else makes the event malformed. Log skipped events with
  `tracing::warn!(uuid, "skipping malformed event")`; the room id is added by the caller's span.

- [ ] **Step 5: Run the gate, expect PASS. Commit** "Read catalogues and events from a room document".

### Task 2: VOTable export (`cocat-core::votable`)

**Files:**
- Create: `crates/cocat-core/src/votable.rs`
- Test: unit tests in the module, plus `bench/correctness/catalogue_api.py` in Task 4

**Interfaces:**
- Consumes: `CatalogueSummary`, `EventRecord` (Task 1).
- Produces: `pub fn export(catalogue: &CatalogueSummary, events: &[EventRecord]) ->
  Result<String, VotableError>`, with `#[derive(Debug, thiserror::Error)] pub enum
  VotableError { #[error("Export VOTable: not all attributes are present in all events {0:?}")]
  MissingAttributes(Vec<String>), #[error("Export VOTable: not all value types are identical
  for all events for attribute {0}")] MixedTypes(String) }` (Python's messages).

The output is what Python's `export_votable` writes for one catalogue, so cocat's
`import_votable` reads it back:

- `<?xml version="1.0" encoding="utf-8"?>`, then `<VOTABLE version="1.4"
  xmlns="http://www.ivoa.net/xml/VOTable/v1.3">`, `<DESCRIPTION>Contact:{author};Name:{name}
  </DESCRIPTION>`, one `<RESOURCE>` with one `<TABLE name="{name with spaces → _}">`,
  `FIELD`s, then `<DATA><TABLEDATA>` with one `<TR>` per event in (start, uuid) order.
- Fixed fields, in this order (attributes as in `VOTABLE_COCAT_FIELDS`):
  `start`: `name="Start Time" ID="TimeIntervalStart" ucd="time.start" datatype="char"
  arraysize="*" xtype="dateTime" utype=""`; `stop`: same with `"Stop Time"`,
  `"TimeIntervalStop"`, `"time.end"`; `author` and `uuid`: `datatype="char" arraysize="*"`;
  `tags` and `products`: `datatype="char" arraysize="*"`, cell is the JSON list; `rating`:
  `datatype="long"`, null written as `0`.
- Then one field per attribute name, sorted, typed from its JSON values: string →
  `char`/`*`, integer → `long`, float → `double`, bool → `boolean`, array →
  `char`/`*`/`utype="json"` with the JSON as the cell. Objects and nulls count as a
  mixed type.
- Text and attribute values XML-escaped (`& < > " '`).
- Rust differs from Python on purpose in one case: a catalogue with no events exports a
  table with the fixed fields and no rows (Python crashes there).

- [ ] **Step 1: Failing unit tests:** `fixed_fields_in_python_order` (field names in
  order), `rating_null_is_zero`, `attribute_types` (one event with
  `{"s":"x","i":1,"f":1.5,"b":true,"l":[1]}` → `char`, `long`, `double`, `boolean`,
  `char`+`utype="json"`), `missing_attribute_is_an_error` (message lists the names),
  `mixed_types_is_an_error`, `escapes_markup` (name `a<b & "c"` round-trips through
  escaping), `empty_catalogue_has_no_rows`.
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement** with `std::fmt::Write`.
- [ ] **Step 4: Gate, expect PASS. Commit** "Export a catalogue as a VOTable".

### Task 3: Read a room without opening it (`RoomManager::read`)

**Files:**
- Modify: `crates/cocat-core/src/store.rs` (read-only loader), `crates/cocat-server/src/room.rs`
- Test: `crates/cocat-server/tests/room_read.rs` (new), unit test in `store.rs`

**Interfaces:**
- Produces:
  - `pub fn read_updates(update_dir: &Path, room_id: &str) -> Result<Vec<Bytes>, StoreError>`
    in `store.rs`: opens the file read-only, returns every complete record, stops at the
    first incomplete or bad one *without* truncating; a missing file is `Ok(vec![])`.
  - `RoomCommand::Read { read: Box<dyn FnOnce(&Doc) + Send> }`.
  - `pub async fn read<T: Send + 'static>(&self, room_id: &str, read: impl FnOnce(&Doc) -> T
    + Send + 'static) -> Result<T, RoomError>` on `RoomManager`.

Behaviour of `read`:
1. Under the registry lock, look the room up.
2. Open: send `Read` with a closure that runs `read` and sends the result on a oneshot.
   If the send fails or the oneshot is dropped (the room unloaded in between), go to 3.
   This needs `read` to be callable on either path, so wrap it in
   `Arc<Mutex<Option<F>>>` or pass it back on failure; the implementer picks.
3. Closed: estimate `file size × MEMORY_PER_FILE_BYTE`; if a budget is set and open
   rooms + in-flight cold reads + this estimate exceed it, return `RoomError::OverBudget`.
   Otherwise add the estimate to a shared `cold_bytes: Arc<AtomicU64>` (also counted by
   `get_or_open`'s budget check), run `read_updates` + apply + `read` in
   `tokio::task::spawn_blocking`, and subtract the estimate when done, panics included (a
   guard with `Drop`).

- [ ] **Step 1: Failing tests:**
  - `store.rs`: `read_updates_leaves_a_torn_tail` — write two records plus half a third,
    `read_updates` returns two, the file length is unchanged.
  - `room_read.rs`: `reads_an_open_room_through_its_thread` (a client attached and wrote
    an update; `read` sees it before any flush); `reads_a_closed_room_from_its_file`
    (afterwards `open_rooms() == 0`); `missing_file_reads_an_empty_doc`;
    `cold_read_over_budget_is_refused`; `cold_read_releases_its_budget` (after a read,
    `get_or_open` of a room that fits only alone succeeds);
    `read_survives_an_unload_race` (detach the last client and `read` concurrently, 100
    iterations; every read returns, with a 5 s timeout per read).
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Gate, expect PASS. Commit** "Read a room without keeping it open".

### Task 4: The routes (`catalogues_api`)

**Files:**
- Create: `crates/cocat-server/src/catalogues_api.rs`
- Modify: `crates/cocat-server/src/api.rs` (merge the router; add
  `ApiError::Unavailable(String)` → 503), `crates/cocat-server/src/lib.rs`
- Test: `crates/cocat-server/tests/catalogues_api.rs`; `bench/correctness/catalogue_api.py`

**Interfaces:**
- Consumes: `RoomManager::read` (Task 3), `model::*` (Task 1), `votable::export` (Task 2),
  the existing membership check in `api.rs` (make it `pub(crate)`).
- Routes:
  - `GET /api/rooms/{room_id}/catalogues` → `{"catalogues": [CatalogueSummary…]}`.
  - `GET /api/rooms/{room_id}/catalogues/{catalogue}` → one `CatalogueSummary`. When
    `{catalogue}` ends in `.votable`, it answers the VOTable for the uuid before the
    suffix instead: `content-type: application/x-votable+xml`, `content-disposition:
    attachment; filename="{uuid}.xml"`; a `VotableError` is 422 with its message.
    (The suffix is handled in the handler because the router can't match part of a
    segment.)
  - `GET /api/rooms/{room_id}/catalogues/{catalogue}/events?start=&stop=&limit=&cursor=`
    → `{"events": [EventRecord…], "next": "<cursor>" | null}`.
- Events query rules: `start`/`stop` parse with `Timestamp::parse` (422 otherwise; 422 when
  start > stop); an event is kept when it *overlaps* the range (`event.stop >= start` and
  `event.start <= stop`), either bound optional. `limit` defaults to 1000, must be 1..=10000
  (422 otherwise). `cursor` is base64url (no padding) of `"{start unix micros}|{uuid}"` for
  the last event returned; the next page starts strictly after that (start, uuid) pair.
  `next` is null on the last page.
- Errors: unknown catalogue → 404 `"Catalogue {uuid} not found"`; `RoomError::OverBudget` →
  503 `"Server memory budget reached"`; other `RoomError` → 500.
- `// simplify:` every request filters and sorts the whole catalogue, and a closed room is
  reloaded for every page. Fine for scripts paging a few times; add a short-lived
  read cache when the UI pages through big closed rooms.

- [ ] **Step 1: Failing Rust tests** (same harness as `api` tests: `tower::oneshot`, seeded
  users, an update dir prepared by writing a room file with `yrs`):
  `lists_catalogues_for_a_viewer`, `non_member_gets_404`, `anonymous_gets_401`,
  `unknown_catalogue_404`, `one_catalogue`, `events_are_sorted_and_iso_utc`,
  `time_range_keeps_overlapping_events`, `start_after_stop_is_422`, `limit_out_of_range_is_422`,
  `paging_with_equal_starts_returns_each_event_once` (25 events, 10 sharing one start,
  limit 7, concatenated pages equal the unpaged list), `bad_cursor_is_422`,
  `votable_suffix_returns_xml` (content type and disposition), `votable_mixed_types_is_422`,
  `rest_read_does_not_open_the_room` (`open_rooms() == 0` after every route),
  `over_budget_is_503`.
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Gate, expect PASS.**
- [ ] **Step 5: Cross-check with Python.** `bench/correctness/catalogue_api.py`, in the
  style of the other correctness scripts (`common.py`, `SERVER=rust` only): build a room
  through `cocat.DB` over a live socket (two catalogues, 50 events with tags, products,
  ratings including null, string/int/float/bool/list attributes), then
  (a) the JSON from `/events` equals the events as `cocat.DB` sees them, after normalising
  times to UTC; (b) `cocat.import_votable_str` on the `.votable` answer, into a fresh
  `DB`, yields the same events (uuid, start, stop, author, tags, products, rating,
  attributes). Prints `PASS`/`FAIL`, exits non-zero on failure. Run it once; paste the
  output in the report.
- [ ] **Step 6: Commit** "Serve catalogues and events read-only over /api".

### Task 5: Docs

- [ ] `README.md`: the four routes, one curl example each, the paging rule and the
  overlap rule. Spec status line: phase 2 done. Commit "Document the catalogue API".
