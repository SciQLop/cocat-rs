# Backlog

## Per-user and per-room quotas

Today `COCAT_ROOM_MAX_MB` and `COCAT_OWNER_MAX_MB` are server-wide: every room and every
owner gets the same allowance. Allow an override per user (owner quota) and per room
(room size), stored in the database, falling back to the environment defaults when unset.
The allowance is computed when a room opens (see the `simplify:` note in
`docs/superpowers/reports/2026-11-06-phase-4b.md`), so a changed quota applies at the next
open; say so in the panel, or reload the open room's allowance on change.

## Administration dashboard

The web UI has no superuser pages; administration is the CLI (`create-user`,
`add-user-to-room`, ...) and the `/users` API. A superuser-only dashboard that shows the
state of the server at a glance: number of users and rooms, total storage and its growth,
open rooms and connected clients, the largest rooms, owners close to their quota, recent
sign-ups, server memory. From there, drill down to users (activate/deactivate, verify,
make superuser, delete), rooms (owner, size, members; change owner; delete) and quotas
(the overrides above). Same-site guard, rate limits and
the access matrix apply like every other UI route; every action goes through the existing
lifecycle functions so open sockets are closed as they are today.

The dashboard's numbers are the basic metrics every deployment gets with no extra setup,
so a lab running its own server sees them without Grafana.

## Metrics export

The same numbers and more (request latency per route, websocket frames in/out, room open
time, flush time, rate-limit refusals, quota refusals) exported for Grafana. A Prometheus
`/metrics` endpoint (the `metrics` + `metrics-exporter-prometheus` crates) is the common
choice; some Grafana setups read Graphite, so decide at planning time between
Prometheus, a Graphite push, or both. The endpoint must not be public (superuser, a
token, or a separate listen address).

## Rich catalogue app in the browser (Leptos)

Browse and edit catalogues live in the browser, with diff and history views. The browser
becomes a real cocat client: yrs compiles to WebAssembly, so a Leptos app (web-first,
`leptos_axum` for server rendering + hydration) can reuse `cocat-core` and most of
`cocat-client` and sync over the same websocket. Mounted under `/app` beside the askama
pages, which stay for accounts, rooms and administration; it reuses their Pico stylesheet.
Needs: a browser websocket transport for `cocat-client` (tokio-tungstenite does not run in
wasm), `cargo-leptos` in CI, a ~1 MB wasm bundle budget. History and rollback views depend
on the `ysr` engine (phase 6). Own phase, design first. Chosen over Dioxus (multi-platform,
which SciQLop's Qt desktop makes moot), 2026-11-06.

## Leads from the attacker campaign (not proven)

See `docs/superpowers/reports/2026-11-06-phase-4b.md`, "Leads not proven": export blocking
an open room, outbound queues bounded in frames not bytes, one OS thread per open room,
rate-limit key including the path.
