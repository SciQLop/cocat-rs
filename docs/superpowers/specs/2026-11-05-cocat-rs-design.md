# cocat-rs — design

Status: draft, 2026-11-05. Supersedes nothing yet; the Python `cocat` stays the
product until each phase below replaces a piece of it.

## Why

cocat's server is Python on top of three layers of bindings to the Rust Yjs engine
(`pycrdt` → `wiredb` → `wire-*`), maintained by one person. That chain pins us
(cocat cannot move past `pycrdt < 0.13` because `wiredb` pins it) and every engine
feature waits for a binding. The goals of the rewrite:

1. **No third-party bindings on the server.** Use `yrs` (and later `ysr`) directly.
2. **Scale:** rooms spread over all cores; lower memory around the engine; a path to a
   disk-backed engine so room size is no longer bounded by RAM.
3. **One data model** (events, catalogues, validation, VOTable) shared by server and
   client, in one Rust crate.
4. **Later, one client**: a Rust core with our own *narrow* Python bindings that
   replaces the Python `cocat` package under the same name and API.

Non-goals: changing the wire protocol (it stays the standard Yjs sync protocol, so
existing pycrdt clients keep working throughout); changing the user-facing features
(phases 0–5 of the Python design define them); Shibboleth.

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

## Architecture

One Cargo workspace:

| Crate | Role |
|---|---|
| `cocat-core` | Data model over a Yrs `Doc` (same layout as Python cocat: root maps `events`, `catalogues`; nested maps; tags/products as maps-as-sets), validation, VOTable import/export, room storage trait |
| `cocat-server` | axum: room websockets (Yjs sync via `yrs::sync`), REST `/api`, auth, web UI |
| `cocat-py` (later) | PyO3/maturin bindings exposing cocat's current *public Python API*, not the CRDT |

Storage behind a trait so the engine can change without touching the server:
`RoomStore::{load, append, compact}`. First implementation: the existing `.y` format
(version header + length-prefixed updates), so the Rust server can open rooms the
Python server wrote. Compaction on room open past a size threshold, archiving the old
file to `history/<room>/<date>.y` (that archive *is* the room history).

Concurrency: one tokio task per room owning its `Doc` (no shared locks); websocket
tasks talk to it through channels. Rooms unload when the last client leaves, as today.

## Compatibility with the Python server (hard requirements)

The Rust server must be a drop-in replacement for the same deployment:

- **Same SQLite database and Alembic schema** (users, room, membership, invitation,
  oauth_account). Rust reads/writes it with `sqlx`; schema changes keep going through
  Alembic until the Python server is retired.
- **Same login cookie:** `fastapi-users` JWT, HS256, `aud = ["fastapi-users:auth"]`,
  `sub` = user id, secret from `<db_path>.secret` / `COCAT_SECRET`.
- **Same password hashes:** pwdlib Argon2 (PHC strings) with bcrypt fallback.
- **Same `.y` room files** and room ids.
- **Same routes and behaviour:** `/room/{id}` websocket, `/auth/*`, `/api/*`, `/ui/*`,
  `COCAT_PROXY_PREFIX`, `COCAT_PUBLIC_URL`, SMTP and OAuth env vars, rate limits,
  viewer write-blocking, kick on removal/role change.
- **The Python test suite is the specification** (decided with the team). Two kinds
  of tests exist in the Python repo:
  - *Subprocess tests* (`tests/test_server.py`, `tests/test_api.py`) start a real
    `cocat serve` through `tests/conftest.py`/`tests/utils.py`. A `COCAT_SERVER_CMD`
    environment variable selects the binary; set to the Rust server, they run
    unchanged.
  - *In-process tests* (`tests/test_routes.py`, `tests/test_ui_*.py`, ...) use
    Starlette's `TestClient` on the Python app and sometimes inspect internals
    (DB session, `Connections`). A `COCAT_TEST_SERVER_URL` mode makes the `client`
    fixture an `httpx` client against a live server; tests that need Python internals
    are marked `python_only` and their behaviour is ported as Rust tests.
  The Python repo runs the suite against Rust in CI (Rust binary built from a pinned
  `cocat-rs` revision). A phase is done when its slice of that suite passes against
  Rust.

## Phases

0. **Spike (go/no-go).** `cocat-server` serving only `/room/{id}`: cookie check,
   membership/role from SQLite, Yjs sync with viewer write-blocking, `.y` storage.
   Measure against the Python server with a 100k-event room and 50 simulated clients:
   RAM, room open time, update fan-out latency, CPU. Check the current SciQLop plugin
   syncs unchanged. Deploy option: the proxy routes `/room/*` to Rust, everything else
   to Python.
1. **Room server in production** behind the proxy split, plus compaction + history
   archive. Python keeps accounts, API, UI.
2. **REST API** (`/api/*` rooms, members, invitations) and auth routes
   (`/auth/*`: login, register, verify, reset, ORCID via `openidconnect`), email
   via `lettre`, rate limiting via `tower_governor`.
3. **Web UI** (server-rendered `askama` + htmx; same pages as the NiceGUI UI). Python
   server retired; Alembic migrations move to `sqlx` migrations at that point.
4. **Rust client + `cocat-py`.** Same public Python API (drop-in for SciQLop and
   notebooks), plus array-returning queries for plotting. Published as `cocat` on
   PyPI with maturin (abi3 wheels, Linux/macOS/Windows). Change notifications reach
   Qt through a queue drained on the main thread.
5. **Engine switch to `ysr`** (disk-backed, history, rollback), when its on-disk
   format is stable — behind `RoomStore`, server first, client after.

## Decisions (with the reason)

- **New repository** `cocat-rs`; the Python repo keeps running and receiving fixes
  until replaced; the `cocat` PyPI name moves here in phase 4.
- **`yrs` now, `ysr` later:** `ysr` is experimental (git-only, format unstable).
- **Own websocket glue** over `yrs::sync` instead of `yrs-axum` (last release
  2025-03, lags `yrs` 0.28).
- **Nested event layout kept** (per-field merge, readable history).
- **Public read-only room is not a CRDT room:** a published static snapshot
  (VOTable/JSON or a compacted `.y`) served over HTTP and cacheable, plus an optional
  query API for very large catalogues.
- **Server-rendered UI** (askama + htmx) rather than a WASM framework: smallest
  moving parts, no JS build.

## Candidate crates (versions checked 2026-11-05)

`yrs` 0.28, `axum` 0.8, `tokio`, `sqlx` 0.9 (sqlite), `jsonwebtoken`, `argon2` 0.6,
`openidconnect` 4.1, `lettre` 0.11, `tower_governor` 0.8, `askama` 0.16,
`pyo3` 0.30 + `maturin` 1.16. Auth is assembled from these (no fastapi-users
equivalent); it gets the same adversarial review the Python auth got.

## Risks

- **Auth re-implementation** is the security-sensitive part; mitigated by porting the
  Python tests and the review findings (Host-header reset links, placeholder
  addresses, OAuth association rules) as explicit tests.
- **Engine memory stays until `ysr`:** the spike must confirm the Rust server is not
  *worse*, and the room-size limit/quotas stay necessary until phase 5.
- **Two servers during the transition:** the proxy split must keep cookies, prefix and
  websocket upgrades consistent.
- **`ysr` maturity** gates phase 5; if it stalls, compaction + archive remains the
  history mechanism.

## Open questions for the team

- Where the repository lives (SciQLop org or personal first).
- Whether phase 2 and 3 are worth it, or Python keeps accounts/UI long term and only
  rooms (and later the client) move to Rust.
