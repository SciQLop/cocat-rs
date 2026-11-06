# Phase 5a — Rust client and its Python package — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Rust client for cocat rooms (yrs, no pycrdt) with a Python package that serves
two users: the SciQLop collaborative-catalogs plugin (tailored for speed: one listener per
room, array queries) and scientists' scripts (the synchronous `log_in` / `get_catalogue` /
`save` API of Python cocat).

**Architecture:** two new workspace crates. `cocat-client` (pure Rust): a `Room` owning a
yrs `Doc`, the document layout read/write on top of `cocat-core::model`, a local cache file
(`cocat-core::store`), and a background sync task (tokio, `tokio-tungstenite`, `yrs::sync`)
that reconnects on its own. `cocat-py` (PyO3 + maturin, mixed Rust/Python layout): the
extension module `cocat_rs._native` plus pure-Python `cocat_rs/api.py` for the script mode
(keyring lives there, as in Python cocat).

**Tech Stack:** `yrs` 0.28 (workspace), `tokio`, `tokio-tungstenite` 0.30 and `reqwest`
(already in the lock through the server), `pyo3` 0.30 + `maturin` 1.16 (design doc),
`numpy` (rust-numpy, the release matching pyo3 0.30; check it resolves before Task 3).
Python side: `keyring`; tests with `pytest` in a scratch venv built with `uv`.

**Spec:** design phase 5a; user decisions 2026-11-06: tailor the API to SciQLop's needs
(we are its only user), keep the scientist script mode, ship under a new package name so
old SciQLop (Python `cocat`) and new SciQLop (`cocat_rs`) coexist against the Rust server.
SciQLop's current usage was mapped from `SciQLop/plugins/collaborative_catalogs/`
(client.py, room.py, cocat_provider.py).

## Global Constraints

- Repo `/home/jeandet/Documents/prog/cocat-rs`, work in the worktree
  `/home/jeandet/Documents/prog/cocat-rs-5a` on branch `phase-5a` (another agent works on
  `main` at the same time); commit per task, never push, never merge. ONE cargo or
  maturin command at a time, in the foreground. Full gate (fmt, clippy -D warnings,
  `cargo test --workspace`, and from Task 3 the pytest suite) before each commit.
- **Document layout is the contract** with Python cocat and the server: root maps
  `"catalogues"` and `"events"` keyed by uuid string; the exact field encoding is what
  `cocat-core/src/model.rs` reads and the Python fixtures under
  `crates/cocat-server/tests/fixtures/python` hold. Writes must produce what Python cocat
  writes (times as the same text form, tags/products/events as maps used as sets, rating
  number or null, attributes a nested map). Extend `cocat-core::model` with the write side
  rather than duplicating it.
- Package: distribution `cocat-rs`, import `cocat_rs`. Never the name `cocat` (phase 5b).
- Times: Python side returns timezone-aware UTC `datetime`; arrays are numpy
  `datetime64[us]`. Accepts naive datetimes as UTC (Python cocat behaviour; check
  `models.py`/`event.py` and copy it).
- Errors: a deleted or unknown object raises `RuntimeError` (as Python cocat); invalid
  input raises `ValueError`.
- Login: `POST {url}/auth/jwt/login` form `username`/`password`, cookie `fastapiusersauth`;
  rooms from `GET {url}/api/rooms`. The websocket is `{url}/room/{room_id}` with the cookie.
  A proxy prefix is part of `url`.
- Out of scope: dynamic filters (`set_dynamic_filter` evaluates Python code), per-object
  callbacks (replaced by the room listener), the `cocat` CLI, porting the SciQLop plugin itself
  (separate repo; a follow-up once this lands).

## The Python surface

```python
import cocat_rs as cc

room = cc.Room.open(cache_dir, room_id)          # local cache only, works offline
room.connect(url, cookie)                        # background sync; returns at once
await room.synced()                              # first full sync done (asyncio)
room.wait_synced(timeout=None)                   # same, blocking (scripts)
room.status                                      # "offline" | "connecting" | "synced" | "unauthorized"
room.set_cookie(cookie)                          # after a re-login on "unauthorized"
room.close()                                     # flush cache, stop sync

room.catalogues() -> list[Catalogue]
room.catalogue(uuid_or_name) -> Catalogue        # RuntimeError when missing
room.event(uuid) -> Event
room.create_catalogue(name=, author=, uuid=None, tags=None, attributes=None, events=None)
room.create_event(start=, stop=, author=, uuid=None, tags=None, products=None, rating=None, attributes=None)
with room.transaction(): ...                     # one update, one listener batch for others

cat.uuid / cat.name (get/set) / cat.author / cat.tags / cat.attributes
cat.events -> set[Event]; cat.add_events(evs); cat.remove_events(evs); cat.delete()
cat.set_attributes(**kw); cat.remove_attributes(keys)
cat.event_arrays() -> {"uuid": list[str], "start": ndarray[datetime64[us]], "stop": ...}
cat.event_attributes(keys=None) -> list[dict]    # same order as event_arrays

ev.uuid / ev.start / ev.stop / ev.range (get/set) / ev.author / ev.rating / ev.tags /
ev.products / ev.attributes; ev.set_attributes(**kw); ev.remove_attributes(keys); ev.delete()

room.listen(listener)                            # remote changes only, on the asyncio loop
                                                 # running at registration; returns a handle
                                                 # whose .close() stops it
```

The library dispatches; clients never switch on a change kind. A listener is any object;
for each change the room calls the method of that name if the object has it, and skips the
change otherwise (checked once at `listen`, not per change):

| Method | Arguments |
|---|---|
| `catalogue_added` | `uuid` |
| `catalogue_removed` | `uuid` |
| `catalogue_renamed` | `uuid, name` |
| `catalogue_attributes` | `uuid, set: dict, removed: list[str]` |
| `events_added` | `catalogue, uuids: list[str]` |
| `events_removed` | `catalogue, uuids: list[str]` |
| `event_range` | `uuid, start, stop` |
| `event_attributes` | `uuid, set: dict, removed: list[str]` |
| `event_removed` | `uuid` |
| `status` | `value` |
| `changes_done` | none: called once after each batch, for a single redraw |

One remote transaction is one batch: changes of the same kind are merged (500 events
pasted at once are one `events_added` call with 500 uuids). This replaces SciQLop's ~4
observers per event with one listener per room. A method that raises is logged with its
traceback and the rest of the batch still runs.

Script mode (`cocat_rs.api`, re-exported at top level), same names and arguments as
Python cocat's `api.py`: `set_config(host=, port=, file_path=, room_id=)`,
`log_in(username=None, password=None, save_credentials=True, connect=True)` (keyring as
today; default room: the owned room named like `names.PERSONAL_ROOM_NAME`, else the first),
`log_out()`, `create_catalogue`, `create_event`, `get_catalogue`, `get_event`,
`refresh()` (= `wait_synced`), `save()` (blocks until every local change is written to the
cache file and acknowledged as sent), `import_votable(path)`, `export_votable(catalogues,
path)`, and the save-on-exit hook.

## Review Focus

1. **A document written by `cocat_rs` that Python cocat reads differently** (time text
   form, rating null vs missing, tags as set-maps) — the cross-client test in Task 3 must
   compare every field both ways.
2. **Callbacks from the wrong thread:** the listener must only ever be called on the loop
   thread (`call_soon_threadsafe`), never from the tokio thread; a callback that raises
   must not kill the sync task.
3. **Local writes while disconnected** survive a restart (cache file) and reach the server
   on reconnect, including when the server restarted in between.
4. **`unauthorized`:** a 403 at the handshake or a 1008 close must stop the reconnect loop
   (no hammering the server, no rate-limit lockout) until `set_cookie`.
5. **Big rooms:** `room.catalogues()` and `event_arrays()` on the 100k-event fixture
   (bench/) in well under a second, measured in the Task 3 report.

---

### Task 1: Write side of the document model (`cocat-core`)

**Files:** `crates/cocat-core/src/model.rs` (or a sibling `model_write.rs`), tests in
`crates/cocat-core/tests/model.rs`.

- [ ] Tests first: create catalogue/event, rename, set/remove attributes, add/remove
  events, delete event (also removes it from every catalogue, as Python cocat does — check
  `event.py`), set range; each test reads back through the existing read functions; one
  test applies a Rust-written update to the Python fixture doc and checks the fixture's
  objects are untouched.
- [ ] Implement, gate, commit "Write the cocat document layout".

### Task 2: `cocat-client` crate

**Files:** create `crates/cocat-client/` (`room.rs`, `sync.rs`, `changes.rs`, `login.rs`).

- [ ] `Room::open(cache_dir, room_id)` loads the cache (`FileRoomStore`), appends every
  local update to it. `connect(url, cookie)` spawns the sync task on a runtime the `Room`
  owns: Yjs sync step 1/2 then updates both ways, exponential backoff 1 → 30 s,
  `Unauthorized` stops the loop. Changes: a yrs `observe_deep` on both roots, remote origin
  only, translated into `Change` values, pushed to a channel.
- [ ] `login(url, user, password) -> cookie` and `rooms(url, cookie)` with `reqwest`.
- [ ] Tests against an in-process `cocat-server` (dev-dependency, as `tests/` already do):
  two `Room`s converge; offline write then connect; server restarted between; 403 gives
  `Unauthorized` and no retry; viewer writes are not accepted by the server and the
  client reports it (whatever the server does today: check `ws.rs`). Gate, commit "Add
  the Rust client".

### Task 3: `cocat-py` (SciQLop surface)

**Files:** create `crates/cocat-py/` (`Cargo.toml` with `crate-type = ["cdylib"]`,
`pyproject.toml` for maturin, `src/lib.rs`, `python/cocat_rs/__init__.py`,
`python/cocat_rs/_native.pyi`, `tests/`).

- [ ] Build into a scratch venv: `uv venv <scratch>/venv && uv pip install maturin pytest
  numpy pytest-asyncio` then `maturin develop -m crates/cocat-py/Cargo.toml`.
- [ ] Tests first (pytest, server = the `cocat-server` binary on a free port, seeded by its
  CLI): the whole surface above; the listener gets one batch per remote transaction on
  the loop thread, same-kind changes merged, `changes_done` once per batch, missing methods
  skipped; a raising method is logged and the rest of the batch and the next batch still run;
  **cross-client**: Python cocat (the existing venv at
  `/home/jeandet/Documents/prog/cocat-python-tests`, read-only, never install into it) and
  `cocat_rs` in the same room, each writes every field, the other reads identical values.
- [ ] Measure `catalogues()` and `event_arrays()` on the bench 100k-event room; write the
  numbers into the commit message. Gate (cargo + pytest), commit "Add the cocat_rs
  Python package".

### Task 4: Script mode

**Files:** `python/cocat_rs/api.py`, `crates/cocat-core/src/votable.rs` (import side),
tests.

- [ ] Port `api.py` behaviour onto `Room` with the same names, arguments and errors; port
  Python cocat's `tests/test_api.py` (keyring monkeypatched with the same in-memory
  wallet) to run against `cocat_rs`.
- [ ] VOTable: export through `cocat-core::votable::export`; import in `cocat-core` with
  `quick-xml` (already a dependency). Round-trip tests: a file exported by Python cocat
  imports into the same events; a file exported by `cocat_rs` imports into Python cocat.
- [ ] README section "Python package" (install, the two modes, the listener). Gate, commit
  "Add the script mode".
