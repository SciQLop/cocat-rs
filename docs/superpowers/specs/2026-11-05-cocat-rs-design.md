# cocat-rs — design

Status: draft, 2026-11-05, revised after review. Supersedes nothing yet; the Python
`cocat` stays the product until each phase below replaces a piece of it.

## Why

cocat's server is Python on top of three layers of bindings to the Rust Yjs engine
(`pycrdt` → `wiredb` → `wire-file`/`wire-websocket`), maintained by one person. That
chain pins us (cocat cannot move past `pycrdt < 0.13` because `wiredb` pins it) and
every engine feature waits for a binding. The goals of the rewrite:

1. **No third-party bindings on the server.** Use `yrs` (and later `ysr`) directly.
2. **Scale:** rooms spread over all cores, and a path to a disk-backed engine so room
   size is no longer bounded by RAM. Phases 0–4 do not reduce memory per room — the
   engine's in-memory representation is the same — the win there is parallelism, CPU and
   maintainability; only phase 5 changes the memory ceiling.
3. **One data model** (events, catalogues, validation, VOTable) shared by server and
   client, in one Rust crate.
4. **Later, one client**: a Rust core with our own *narrow* Python bindings that
   replaces the Python `cocat` package under the same name and API.

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
| `cocat-server` | axum: room websockets (Yjs sync via `yrs::sync`), REST `/api`, auth, web UI |
| `cocat-py` (later) | PyO3/maturin bindings exposing cocat's current *public Python API*, not the CRDT |

### Room storage

One concrete `FileRoomStore` for now (`load`, `append`, `flush`), no trait: the trait
earns its keep only when a second engine exists (phase 5). It speaks the existing `.y`
format — `"0.0.1"` + `\0` header, then `varuint(len) + update-v1` records — so Rust
opens rooms Python wrote and Python opens rooms Rust wrote.

Phase 1 is append-only. Recovery: read records until one fails, truncate to the last
good offset, log it; a crash mid-append must not make the room unreadable. Durability:
flush before a room unloads and on SIGINT/SIGTERM, and fsync per flush — the file is the
only copy.

Compaction and `history/<room>/<date>.y` are a later item, not phase 1, and gated on a
retention/deletion policy: an archive is a full Yjs history, deleted events and author
fields included.

### Who owns a room

From phase 1 the Rust process owns a room: its sockets, its file, its lifecycle. The
room-lifecycle mutations move with it (member remove, role change, ownership transfer,
room delete, invitation accept). Python keeps accounts, the rest of `/api`, and the UI.

Reason: the Python kick mechanism is an in-process registry of sockets. Across a split
it closes nothing, so a removed member keeps writing and a deleted room's file is
rewritten by the still-live room. One owner per room is the only version of the split
that keeps the anti-goal "a removed member keeps reading or writing the room" true.

Fallback if the move slips: Python calls a private admin endpoint on Rust (shared
secret, localhost only) to close sockets and drop the room.

### Concurrency and memory

One tokio task per room owning its `Doc` (no shared locks); websocket tasks talk to it
through channels. Rooms unload when the last client leaves, as today.

A room costs ~580 MB at 100k events, so opening more rooms means more RAM: a budget
(`COCAT_ROOM_MEMORY_BUDGET`, derived from a measured bytes/event) with an admission rule
— unload least-recently-used rooms with no clients first, refuse a new room beyond the
budget with a clear close reason.

## Compatibility with the Python server (hard requirements)

The Rust server must be a drop-in replacement for the same deployment:

- **Same SQLite database and Alembic schema** (user, room, membership, invitation,
  oauth_account). Rust reads/writes it with `sqlx` and **never migrates**: it checks
  `alembic_version` at startup and refuses an unknown revision. Keep transactions short
  and never hold a read transaction across an `await` (the Python suite asserts no
  connection is held while a socket is open). Alembic stays the migration tool, run as
  `cocat migrate` from a pinned Python env, until the Python package is retired; moving
  to sqlx instead would need a baseline row in `_sqlx_migrations` for existing databases.
- **Same login cookie:** `fastapi-users` JWT, HS256, `aud = ["fastapi-users:auth"]`,
  `sub` = user id (a string UUID), `exp` required, secret from `<db_path>.secret` /
  `COCAT_SECRET`; cookie `fastapiusersauth`, path `/`, `Secure`, `HttpOnly`,
  `SameSite=lax`, session cookie; the user must be active, and verified when a mailer
  is configured. Password-reset and verification tokens are JWTs signed with the same
  secret (`aud = "fastapi-users:reset"` / `"fastapi-users:verify"`, 3600 s), so links
  issued before the cutover keep working.
- **Same password hashes:** pwdlib Argon2 (PHC strings) with bcrypt fallback; a bcrypt
  hash is rehashed to Argon2 on login.
- **Same invitation tokens:** `secrets.token_urlsafe(32)`, stored as the sha256 hex in
  `invitation.token_hash`.
- **Same storage formats:** UUIDs as `CHAR(36)` with dashes, roles as the enum names in
  a `VARCHAR` + `CHECK`, timestamps as naive-UTC `YYYY-MM-DD HH:MM:SS.ffffff`. The
  `accesstoken` table is unused.
- **Same `.y` room files** and room ids (`[A-Za-z0-9_-]{1,64}`; new rooms are uuid4 hex).
- **Same routes and behaviour:** `/room/{id}` websocket, `/auth/*`, `/api/*`, `/ui/*`,
  `COCAT_PROXY_PREFIX`, `COCAT_PUBLIC_URL`, SMTP and OAuth env vars, rate limits,
  viewer write-blocking, kick on removal/role change.
- **Same rate limits:** a fixed window keyed `path|client_ip`, 10 calls / 60 s per
  route, `COCAT_TRUSTED_PROXIES` hops on `X-Forwarded-For`, 10 000 keys, plain 429.
  Hand-rolled (~40 lines); `tower_governor` has different semantics and would need the
  key derivation written anyway.
- **Same websocket handshake:** a refused client is accepted and then closed with code
  1008, never rejected at the upgrade. Non-SYNC frames are ignored, as today.

### The proxy split

Only a websocket upgrade on exactly `/room/{id}` goes to Rust (`map $http_upgrade`, with
`Upgrade`/`Connection` forwarded). `GET /room/{id}/users` and `GET /rooms` are Python
HTTP routes under the same prefix and stay on Python. The proxy strips
`COCAT_PROXY_PREFIX`, so Rust routes on the stripped path and only generates URLs with
the prefix. Both backends share the cookie. Raise the proxy's read timeout for the room
location (or add ping/pong) so idle rooms are not cut.

## How the Python suite drives Rust

What exists in the Python repo today:

- `tests/test_server.py`: five tests start a real `cocat serve` (origin, api,
  grant-self, room-users, ui-served); one is a pure wire test with no cocat server; two
  test `wait_for_server`.
- `tests/test_routes.py`: 31 tests against an in-process `TestClient` (`app_client`
  fixture). This is the behavioural spec: viewer blocking, kicks, invitations, roles,
  1008s.
- `tests/test_ui_*.py`: ~200 tests drive NiceGUI's in-process simulator. They cannot run
  against a live server and cannot be reused by a Rust UI.
- `tests/test_api.py`: six tests of the Python *client* library (keyring monkeypatched);
  it is the client parity test for phase 4.
- `tests/utils.py` seeds users and rooms by shelling out to the Python `cocat` CLI, so
  the Python package stays installed in CI even when the server binary is Rust.

Work items in the Python repo before Rust can be tested by it:

- `COCAT_SERVER_CMD` in `tests/conftest.py` (it hardcodes `["cocat", "serve", ...]`).
- A `COCAT_TEST_SERVER_URL` mode for `app_client`: `httpx` plus a real websocket client
  against a live server, one server per test database, keeping the `Secure`-cookie
  workaround.
- `python_only` marks for tests that read Python internals (`db.engine.pool`,
  `RecordingMail`); the pool invariant ("no connection held while a socket is open")
  becomes a Rust test.
- CI runs Python plus a `cocat-rs` build pinned to a revision.

A phase is done when its slice of that suite passes against the Rust binary.

## Phases

Order: 0 → 1 → 2 → 4a → (3, if the open question says yes) → 4b → 5.

0. **Spike (go/no-go).** `cocat-server` serving only `/room/{id}`: cookie check,
   membership/role from SQLite, Yjs sync with viewer write-blocking, `.y` storage.
   Workload: one 100k-event room (the 38 MB file), 50 simulated clients, one edit per
   second for 5 minutes, plus a cold open. Run it once behind the real proxy.

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
1. **Room server in production** behind the proxy split: `/room/{id}`, viewer blocking,
   the room-lifecycle mutations, append-only storage with tail recovery and flush.
   Python keeps accounts, the rest of `/api`, and the UI.
2. **REST API and auth:** the remaining `/api/*` routes, `/auth/*` (login, register,
   verify, reset, ORCID via `openidconnect`), email via `lettre`, the hand-rolled rate
   limiter. Python keeps the UI.
3. **Web UI** (if the open question says yes): server-rendered `askama` + htmx, same
   pages as the NiceGUI UI. Starts with a new HTTP-level acceptance suite, because the
   NiceGUI tests cannot be reused. Python server retired here.
4. **Rust client + `cocat-py`.**
   - **4a:** the document/catalogue API SciQLop uses, plus array-returning queries for
     plotting. This is the phase that removes the `pycrdt` pin.
   - **4b:** the rest of the public Python API (VOTable, filters, keyring/session, CLI).
     The `cocat` PyPI name moves to Rust here — a one-way door, taken only after the
     Python wheel is retired.
5. **Engine switch to `ysr`** (disk-backed, history, rollback), when its on-disk format
   is stable — server first, client after. `y-crdt/ysr-persistence` and
   `y-crdt/pycrdt-store` are the fallback paths if `ysr` stalls.

Not a phase, but required before the cutover: a **rollback drill** — flip the proxy to
Rust, flip back, verify rooms open in Python with the same state.

## Decisions (with the reason)

- **New repository** `cocat-rs`; the Python repo keeps running and receiving fixes
  until replaced.
- **`yrs` now, `ysr` later:** `ysr` is experimental (git-only, format unstable).
- **Own websocket glue** over `yrs::sync` instead of `yrs-axum` (last release
  2025-03, lags `yrs` 0.28).
- **Nested event layout kept** (per-field merge, readable history).
- **Awareness is a later feature**, not part of the port: parity means ignoring
  non-SYNC frames.
- **Concrete file store now**, trait in phase 5.
- **Hand-rolled rate limiter** instead of `tower_governor` (same semantics as Python,
  no key-derivation rewrite).
- **Alembic stays** the migration tool; Rust validates the revision and never migrates.
- **Public read-only room is not a CRDT room:** a sanitized VOTable/JSON snapshot served
  over HTTP and cacheable, in its own later phase. Never the raw `.y` — it carries
  tombstones and author fields.
- **Server-rendered UI** (askama + htmx) if the UI is ported: no JS build step, but htmx
  is still a vendored JS file, pinned like any dependency.

## Implementation notes

- `yrs::Update` is `!Send`: encode on the room task and send `Bytes` through channels.
- `Encode::encode_v1` has no length prefix: prepend `varuint(len)` to match
  `write_message`, and read `varuint` then slice when loading.
- Create the `.y` (header only) when a room opens.
- `Awareness::new` owns the `Doc`; the room task holds it.
- CLI: `serve --host --port --update_dir --db_path`, the flags the Python fixture
  passes; answer `/` (404 is fine) so `wait_for_server` stops retrying; flush and exit 0
  on SIGINT.

## Candidate crates (versions checked 2026-11-05)

`yrs` 0.28, `axum` 0.8, `tokio`, `sqlx` 0.9 (sqlite), `jsonwebtoken`, `argon2` 0.6,
`openidconnect` 4.1, `lettre` 0.11, `askama` 0.16, `pyo3` 0.30 + `maturin` 1.16. Auth is
assembled from these (no fastapi-users equivalent); it gets the same adversarial review
the Python auth got.

## Risks

- **Auth re-implementation** is the security-sensitive part; mitigated by porting the
  Python tests and the review findings (Host-header reset links, placeholder addresses,
  OAuth association rules) as explicit tests.
- **Engine memory stays until `ysr`:** phases 0–4 do not reduce memory per room, and more
  open rooms means more RAM; the memory budget and admission rule above, plus quotas,
  hold the line until phase 5.
- **Durability:** the `.y` file is the only copy; flush/fsync policy and tail recovery
  are part of phase 1, compaction is not.
- **Two servers during the transition:** one owner per room (phase 1 moves the lifecycle
  mutations), plus the exact-path proxy rule, shared cookie, prefix and timeouts.
- **`ysr` maturity** gates phase 5; if it stalls, compaction + archive (with the
  retention policy) is the history mechanism.

## Open questions for the team

- Where the repository lives (SciQLop org or personal first).
- **Phase 2 (accounts/API):** the Rust auth is a security-sensitive rewrite of code that
  is already tested and reviewed; if accounts stay in Python, the split ends at the room
  boundary and phases 3/4b shrink.
- **Phase 3 (UI):** ~200 NiceGUI tests run in-process and cannot be reused, and the UI
  is not part of the binding problem; if it is ported, it starts with a new HTTP-level
  acceptance suite.
