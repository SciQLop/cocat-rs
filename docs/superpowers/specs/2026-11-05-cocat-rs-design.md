# cocat-rs — design

Status: draft, 2026-11-05, revised after review and the team decisions. The Python
`cocat` keeps running until the Rust server is feature-complete; there is no side-by-side
production phase.

## Why

cocat's server is Python on top of three layers of bindings to the Rust Yjs engine
(`pycrdt` → `wiredb` → `wire-file`/`wire-websocket`), maintained by one person. That
chain pins us (cocat cannot move past `pycrdt < 0.13` because `wiredb` pins it) and
every engine feature waits for a binding. The goals of the rewrite:

1. **No third-party bindings on the server.** Use `yrs` (and later `ysr`) directly.
2. **Scale:** rooms spread over all cores, and a path to a disk-backed engine so room
   size is no longer bounded by RAM. Phases 0–5 do not reduce memory per room — the
   engine's in-memory representation is the same — the win there is parallelism, CPU and
   maintainability; only phase 6 changes the memory ceiling.
3. **One data model** (events, catalogues, validation, VOTable) shared by server and
   client, in one Rust crate.
4. **One client**: a Rust core with our own *narrow* Python bindings that replaces the
   Python `cocat` package under the same name and API.

Non-goals: changing the wire protocol (it stays the standard Yjs sync protocol, so
existing pycrdt clients keep working throughout); changing the user-facing features
(phases 0–5 of the Python design define them); relaying awareness; Shibboleth.

## What we measured (Python cocat 0.15, one room, 2026-11-05)

| 100 catalogues × 1 000 events | |
|---|---|
| `.y` file | 38 MB (~380 B/event); compaction only saves ~3 % |
| server RAM for the open room (Yrs `Doc` alone) | ~580 MB (~6 KB/event) |
| room open (replay of the file) | ~1 s |
| full state sent to a new client | 36 MB |
| one field edit | +55 B in the log, +13 B after compaction |
| a deleted event | ~95 B remain forever |
| `db.events` in the Python API | 0.7 s per call |

Consequences: the engine's in-memory representation is the RAM ceiling (Rust alone
does not change it); a disk-backed engine (`ysr`) does. Flattening the event layout
saves little (−22 % RAM) and loses per-field merge, so the nested layout stays.
Compaction saves ~3 % and cannot reclaim tombstones, so rewriting the only copy of a
room is not worth it yet.

## Architecture

One Cargo workspace:

| Crate | Role |
|---|---|
| `cocat-core` | Data model over a Yrs `Doc` (same layout as Python cocat: root maps `events`, `catalogues`; nested maps; tags/products as maps-as-sets), validation, VOTable import/export, room file storage |
| `cocat-server` | axum: room websockets (Yjs sync via `yrs::sync`), REST `/api`, auth, web UI, CLI, sqlx schema |
| `cocat-py` (later) | PyO3/maturin bindings exposing cocat's current *public Python API*, not the CRDT |

### Room storage

One concrete `FileRoomStore` for now (`load`, `append`, `flush`), no trait: the trait
earns its keep only when a second engine exists (phase 6). It speaks the existing `.y`
format — `"0.0.1"` + `\0` header, then `varuint(len) + update-v1` records — so the
import tool reads Python rooms directly.

Append-only. Recovery: read records until one fails, truncate to the last good offset,
log it; a crash mid-append must not make the room unreadable. Durability: flush before a
room unloads and on SIGINT/SIGTERM, and fsync per flush — the file is the only copy.

Compaction and `history/<room>/<date>.y` are a later item, gated on a retention/deletion
policy: an archive is a full Yjs history, deleted events and author fields included.

### Concurrency and memory

One tokio task per room owning its `Doc` (no shared locks); websocket tasks talk to it
through channels. Rooms unload when the last client leaves, as today.

A room costs ~580 MB at 100k events, so opening more rooms means more RAM: a budget
(`COCAT_ROOM_MEMORY_BUDGET`, derived from a measured bytes/event) with an admission rule
— unload least-recently-used rooms with no clients first, refuse a new room beyond the
budget with a clear close reason.

## The HTTP and websocket contract

The SciQLop plugin and the Python `cocat` client are what we must not break. They depend
on:

- **Routes and payloads as today:** `/room/{id}` (websocket), `/rooms`, `/room/{id}/users`,
  `/api/*`, `/auth/*`, `/ui/*`, and the JSON shapes and status codes of the Python API
  tests.
- **The login cookie:** name `fastapiusersauth`, path `/`, `Secure`, `HttpOnly`,
  `SameSite=lax`, session cookie. The name and attributes are the contract (the client
  reads it by name); the token format is ours, with no live sessions to preserve.
- **Websocket behaviour:** a refused client is accepted and then closed with code 1008,
  never rejected at the upgrade; viewer updates are dropped but sync requests answered;
  removal, role change and room deletion close that user's socket; non-SYNC frames are
  ignored.
- **Room ids** `[A-Za-z0-9_-]{1,64}`, and `.y` files that pycrdt can reopen.
- **Environment variables:** `COCAT_PROXY_PREFIX`, `COCAT_PUBLIC_URL`, `COCAT_SECRET`,
  `COCAT_TRUSTED_PROXIES`, SMTP and OAuth settings. Behind the proxy, Rust routes on the
  stripped path and generates URLs with the prefix.
- **Rate limits:** a fixed window keyed `path|client_ip`, 10 calls / 60 s per route,
  `COCAT_TRUSTED_PROXIES` hops on `X-Forwarded-For`, 10 000 keys, plain 429. Hand-rolled
  (~40 lines); `tower_governor` has different semantics and would need the key derivation
  written anyway.

Rust owns its schema (sqlx migrations from day one). No compatibility with the Python
database is required beyond what the import tool reads.

### Read-only catalogue API (new)

Origin: `erdoganfurkan/cocat@missing_rest_api` (2026-05) adds `GET
/rooms/{id}/catalogues`, `GET /catalogues/{uuid}` and `GET /catalogues/{uuid}/events`,
so scripts and web pages can read catalogues without a CRDT client. The need is real
(and is the query side of the public read-only room); the branch itself is not
mergeable — it predates the rooms model (`user.rooms`), opens a room with no client that
then never unloads (wiredb only unloads on last-client-leave), scans every room of the
user to find a uuid, drops `products`/`rating`, truncates timestamps to naive seconds,
and has no paging. We take the routes' intent, not the code:

- `GET /api/rooms/{id}/catalogues` → uuid, name, author, tags, attributes, event count.
- `GET /api/rooms/{id}/catalogues/{uuid}` → the same for one catalogue.
- `GET /api/rooms/{id}/catalogues/{uuid}/events?start=&stop=&limit=&cursor=` → full
  events (uuid, start, stop, author, tags, products, rating, attributes), sorted by
  start, ISO 8601 UTC with microseconds, time-range filtered, paged.
- `GET /api/rooms/{id}/catalogues/{uuid}.votable` → VOTable export (same writer as
  `cocat-core`).
- Scoped by room (no global uuid scan); any member, viewer included; non-members get the
  same 404 as every other room route.
- Reads go through the room task when the room is open, and otherwise load the `.y`
  read-only into a short-lived `Doc` that is dropped after the request (never registered
  as an open room), so REST reads cannot pin rooms in memory. Large rooms hit the memory
  budget like any other open.
- The fork's tests (`tests/test_rest_catalogues.py`) are the starting acceptance cases,
  adapted to these routes; credit the author in the changelog.

## How the Python suite drives Rust

What exists in the Python repo today:

- `tests/test_server.py`: five tests start a real `cocat serve` (origin, api,
  grant-self, room-users, ui-served); one is a pure wire test with no cocat server; two
  test `wait_for_server`.
- `tests/test_routes.py`: 31 tests against an in-process `TestClient` (`app_client`
  fixture). This is the behavioural spec: viewer blocking, kicks, invitations, roles,
  1008s.
- `tests/test_ui_*.py`: ~200 tests drive NiceGUI's in-process simulator. They cannot run
  against a live server and cannot be reused by the Rust UI.
- `tests/test_api.py`: six tests of the Python *client* library (keyring monkeypatched);
  it is the client parity test for phase 5.
- `tests/utils.py` seeds users and rooms by shelling out to a `cocat` CLI.

Work items in the Python repo before Rust can be tested by it:

- `COCAT_SERVER_CMD` in `tests/conftest.py` (it hardcodes `["cocat", "serve", ...]`).
- A `COCAT_TEST_SERVER_URL` mode for `app_client`: `httpx` plus a real websocket client
  against a live server, one server per test database, keeping the `Secure`-cookie
  workaround.
- `python_only` marks for tests that read Python internals (`db.engine.pool`,
  `RecordingMail`); the pool invariant ("no connection held while a socket is open")
  becomes a Rust test.

The Rust CLI provides the same subcommands the fixtures use — `serve --host --port
--update_dir --db_path`, `create-user --email --password --db_path`, `create-room --owner
--room-id --db_path`, `add-user-to-room --email --room_id --db_path` — so the suite
seeds a Rust server without the Python package. A phase is done when its slice passes
against the Rust binary.

## Import (one-shot, before the cutover)

`cocat-server import --update-dir <dir> --db <path> [--users <python.db>] [--owner <email>]`

- **Rooms** from the `.y` files: every valid file becomes a room (id from the file name,
  name = id), left in place.
- **Users, memberships, invitations** from the Python SQLite when `--users` is given:
  Argon2 PHC hashes verify as-is, bcrypt hashes rehash on first login; roles and
  memberships are copied; without `--users`, `--owner` names the owner of the imported
  rooms.
- Idempotent and `--dry-run`; the Python database is opened read-only and kept until the
  import is verified.

## Phases

0. **Spike (go/no-go).** `cocat-server` serving only `/room/{id}`: cookie check,
   membership/role from SQLite, Yjs sync with viewer write-blocking, `.y` storage.
   Workload: one 100k-event room (the 38 MB file), 50 simulated clients, one edit per
   second for 5 minutes, plus a cold open.

   | Gate | GO |
   |---|---|
   | Wire parity | Same SYNC_STEP1 reply and broadcast bytes as Python; viewer updates dropped, sync answered |
   | File parity | Opens the Python 38 MB `.y`; pycrdt reopens Rust's file with the same state |
   | Refusal | Upgrade accepted, then closed 1008 |
   | Suite | The converted websocket slice of `test_routes.py` (~10 tests) green through `COCAT_TEST_SERVER_URL` |
   | Client | `cocat.api` + `wire_websocket` (the SciQLop path) sync and reopen identically |
   | Fuzz | 2 clients × 10k random ops converge to Python's state hash |
   | Room open, cold, p95 | ≤ 1.5 s |
   | Fan-out, 50 clients | p50 ≤ 10 ms, p99 ≤ 50 ms |
   | RSS, one open room | ≤ 600 MB (Python: 580 MB) |
   | CPU, same run | ≤ 0.6× Python |
   | Scale-out, 8 rooms × 8 clients | ≥ 4× single-room throughput and ≥ 1.5× Python's 8-room |
   | Soak, 1 h | < 5 % RSS drift, no lost update after SIGINT + restart |
   | Open/close ×20 | RSS back within 5 % |

   NO-GO if any correctness gate fails, fan-out p99 > 200 ms, RSS > 640 MB, any lost
   update, drift > 10 %, or scale-out < 1.5× Python (that last one fails the premise, not
   the implementation). Timebox 10 days; stop at day 5 if wire and file parity are not
   green. Publish the numbers in the repo.
1. **Rooms and lifecycle.** `/room/{id}`, viewer blocking, kick on removal/role
   change/room delete, the lifecycle mutations that drive them, login (the cookie every
   socket needs), append-only storage with tail recovery and flush, the sqlx schema and
   the seeding CLI.
2. **Auth and the REST API.** Register, verify, reset, ORCID via `openidconnect`, email
   via `lettre`, the rate limiter, rooms/members/invitations under `/api/*`, and the
   read-only catalogue API.
3. **Web UI.** Server-rendered `askama` + htmx, same pages as the NiceGUI UI. Starts
   with a new HTTP-level acceptance suite, because the NiceGUI tests cannot be reused.
4. **Import tool**, run as the cutover step, then the Rust server ships.
5. **Rust client + `cocat-py`.**
   - **5a:** the document/catalogue API SciQLop uses, plus array-returning queries for
     plotting. This is the phase that removes the `pycrdt` pin.
   - **5b:** the rest of the public Python API (VOTable, filters, keyring/session, CLI).
     The `cocat` PyPI name moves to Rust here — a one-way door, taken only after the
     Python wheel is retired.
6. **Engine switch to `ysr`** (disk-backed, history, rollback), when its on-disk format
   is stable — server first, client after. `y-crdt/ysr-persistence` and
   `y-crdt/pycrdt-store` are the fallback paths if `ysr` stalls.

The server ships when it is feature-complete for the contract above (rooms, API, auth,
UI); Python is retired at the same cutover, with the imported data kept until verified.

## Decisions (with the reason)

- **New repository** `cocat-rs`, in the SciQLop GitHub organisation. Python keeps running
  and receiving fixes until the cutover.
- **Rust replaces the server wholesale**, no side-by-side phase: one process owns each
  room (sockets, file, lifecycle), so kicks and deletions need no cross-process channel.
- **Rust owns its schema** with sqlx migrations from day one.
- **`yrs` now, `ysr` later:** `ysr` is experimental (git-only, format unstable).
- **Own websocket glue** over `yrs::sync` instead of `yrs-axum` (last release 2025-03,
  lags `yrs` 0.28).
- **Nested event layout kept** (per-field merge, readable history).
- **Awareness is a later feature**, not part of the port: parity means ignoring non-SYNC
  frames.
- **Concrete file store now**, trait in phase 6.
- **Hand-rolled rate limiter** instead of `tower_governor` (same semantics as Python, no
  key-derivation rewrite).
- **Public read-only room is not a CRDT room:** a sanitized VOTable/JSON snapshot served
  over HTTP and cacheable, in its own later phase. Never the raw `.y` — it carries
  tombstones and author fields. Its query side reuses the read-only catalogue API.
- **Server-rendered UI** (askama + htmx): no JS build step, but htmx is still a vendored
  JS file, pinned like any dependency.

## Implementation notes

- `yrs::Update` is `!Send`: encode on the room task and send `Bytes` through channels.
- `Encode::encode_v1` has no length prefix: prepend `varuint(len)` to match
  `write_message`, and read `varuint` then slice when loading.
- Create the `.y` (header only) when a room opens.
- `Awareness::new` owns the `Doc`; the room task holds it.
- `serve` answers `/` (404 is fine) so `wait_for_server` stops retrying, and flushes and
  exits 0 on SIGINT.

## Candidate crates (versions checked 2026-11-05)

`yrs` 0.28, `axum` 0.8, `tokio`, `sqlx` 0.9 (sqlite), `jsonwebtoken`, `argon2` 0.6,
`openidconnect` 4.1, `lettre` 0.11, `askama` 0.16, `pyo3` 0.30 + `maturin` 1.16. Auth is
assembled from these (no fastapi-users equivalent); it gets the same adversarial review
the Python auth got.

## Risks

- **Auth re-implementation** is the security-sensitive part; mitigated by porting the
  Python tests and the review findings (Host-header reset links, placeholder addresses,
  OAuth association rules) as explicit tests.
- **Engine memory stays until `ysr`:** phases 0–5 do not reduce memory per room, and more
  open rooms means more RAM; the memory budget and admission rule above, plus quotas,
  hold the line until phase 6.
- **Durability:** the `.y` file is the only copy; flush/fsync policy and tail recovery
  are part of phase 1, compaction is not.
- **Import is one-shot:** a wrong mapping is hard to undo; dry-run first, keep the Python
  database and update directory until the import is verified.
- **`ysr` maturity** gates phase 6; if it stalls, compaction + archive (with the
  retention policy) is the history mechanism.
