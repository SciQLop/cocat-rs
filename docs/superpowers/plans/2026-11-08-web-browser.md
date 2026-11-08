# Web catalogue browser — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Leptos page, opened from a room, that holds a live replica of the room, browses and
edits its catalogues, shows who is there, and shows the room's history with diffs and restore.

**Architecture:** A new crate `crates/cocat-web` (Leptos 0.8, client-side rendered, built by
`trunk` to a static bundle). It runs `cocat-core` in the browser over a `yrs::Doc` synced with
the server's websocket (`cocat.v2`, the session cookie). `cocat-server` serves the bundle from
a directory (`COCAT_WEB_DIR`) behind the login, and relays Yjs awareness for presence. History
comes from the routes of the history plan.

**Tech Stack:** Leptos 0.8 (`csr`), trunk 0.22, wasm-bindgen, web-sys / gloo-net (WebSocket with
subprotocol, fetch), yrs 0.28 (`yrs::sync` messages and `Awareness`), cocat-core.

**Spec:** `docs/superpowers/specs/2026-11-08-web-catalogue-browser-design.md`. Mockups: the canvas
linked at its top. Depends on `docs/superpowers/plans/2026-11-08-history-server.md` (Tasks 4–5) for
the history UI only.

## Global Constraints

- Everything in `h-rules.md` (TDD, one build at a time in the foreground, the gate, commits on
  `main`, no push, no machine paths).
- Look: the existing stylesheet's tokens (ink `#1b2230`, muted `#5b6475`, rule `#e2e5ea`, accent
  `#2753b8`, amber `#f0ad2e`), Inter or the system font, monospace for times, the CoCat mark;
  works at 390 px wide (side panels stack under the table).
- Reads and writes go through `cocat_core::model` / `rules` in the browser; never a REST write.
- A viewer's page is read-only: no edit controls render, and the server ignores its updates anyway.
- Logic the UI needs (rows from the doc, filtering, sorting, formatting durations, diff
  rendering data) lives in plain Rust modules of `cocat-web` with native `cargo test` unit tests;
  view code stays thin. Each UI task ends with headless Firefox screenshots at 1440 and 390 px
  against a local server (the method in the task) and the lead looks at them.
- The workspace's native gate must not need the wasm toolchain: `cocat-web` is a workspace member
  whose `cargo test` runs its logic tests natively; the wasm build is `trunk build` (and a CI job).

## Review Focus

- A dropped websocket (server restart, network): the page says it is reconnecting, keeps local
  edits, and resyncs them; it never shows a stale "Live".
- Two people editing the same event field at once: both pages converge to the same value.
- A catalogue of 10k+ events: scrolling stays smooth (virtualised rows), filter typing is not
  blocked for more than a frame or two.
- A smart catalogue whose rule is invalid after a colleague's edit: the page shows the rule error,
  not a blank table or a panic.
- A session that expires while the page is open: the page sends the user to sign in and back.

---

### Task 1: cocat-core in the browser, and the crate

**Files:**
- Modify: `crates/cocat-core/Cargo.toml` (uuid `js` feature on `wasm32`), workspace `Cargo.toml` (member),
  `.github/workflows/ci.yml` (job: install `wasm32-unknown-unknown` and trunk, `trunk build --release` in
  `crates/cocat-web`)
- Create: `crates/cocat-web/{Cargo.toml,Trunk.toml,index.html,src/main.rs,src/lib.rs,style.css}`

- [ ] `cargo check -p cocat-core --target wasm32-unknown-unknown` fails today on uuid's randomness; make it pass.
- [ ] A Leptos CSR app that renders the CoCat header (mark, "cocat", breadcrumb) and "Loading room…";
  `trunk build --release` produces `dist/` with hashed file names and `public_url = "./"` so it works
  under any prefix.
- [ ] Gate (native) and `trunk build` both pass. Commit "cocat-web: the crate and cocat-core on wasm".

### Task 2: The server serves the app and relays awareness

**Files:**
- Modify: `crates/cocat-server/src/ui/mod.rs` and a new `crates/cocat-server/src/ui/app.rs`,
  `crates/cocat-server/src/room.rs` (awareness), `crates/cocat-server/templates/room.html` (an "Open
  catalogues" button), `docker/Dockerfile` (build the bundle with trunk, copy `dist` to
  `/usr/share/cocat/web`, set `COCAT_WEB_DIR`), `docs/deploy.md`
- Test: `crates/cocat-server/tests/ui_app.rs`, `crates/cocat-server/tests/awareness.rs`, access matrix rows

**Interfaces:**
- `GET /ui/rooms/{room_id}/browse` → login required (redirect like other UI pages), membership required
  (the room's 404 page otherwise); answers the bundle's `index.html` with `<base href>` set to
  `<prefix>/ui/app/` and a `<meta name="cocat-room" content="{room_id}">`, `<meta name="cocat-role">`,
  `<meta name="cocat-ws" content="<public ws url for the room>">`, `<meta name="cocat-api" content="<prefix>/api">`,
  `<meta name="cocat-user" content="<email>">` (all HTML-escaped).
- `GET /ui/app/{*path}` → files under `COCAT_WEB_DIR` (no directory listing, no `..`), long cache for hashed
  names; when `COCAT_WEB_DIR` is unset or missing, `/browse` answers 503 "The catalogue browser is not
  installed on this server".
- Awareness: the room relays `Message::Awareness` frames to the other clients of the room. Before relaying, the
  server rewrites each client state's `user` field to `{ "email": <account email>, "color": <hex derived
  from a hash of the email> }`; a client may only update its own awareness client id (the first id it
  announced). States of a detached client are removed (an awareness update with `null` state) for the others.

- [ ] Failing tests: `browse_requires_login`; `browse_for_a_stranger_is_404`; `browse_serves_index_with_the_room_meta`;
  `app_files_are_served_and_dotdot_is_refused`; `without_a_web_dir_browse_says_not_installed`;
  `awareness_is_relayed_with_the_server_identity` (two ws clients, one claims another email → the other sees
  the real one); `a_closed_client_disappears_from_awareness`.
- [ ] Implement, gate, commit "Serve the catalogue browser and relay presence".

### Task 3: Live replica and the catalogue list

**Files:**
- Create: `crates/cocat-web/src/sync.rs` (websocket + Yjs sync loop), `src/session.rs` (meta tags → config),
  `src/state.rs` (the `Doc` in an `Rc`, a `revision` signal bumped on each applied or local update),
  `src/views/{header.rs,sidebar.rs}`

**Interfaces:**
- `struct RoomConfig { room_id, role, ws_url, api_url, user_email }` read from the meta tags.
- Sync: open with protocol `cocat.v2`; send SyncStep1; apply SyncStep2/Update with origin `"remote"`; local
  transactions (any other origin) are encoded and sent as `Update`; on close, show "Reconnecting…" and retry with
  backoff (1 s doubling to 30 s), resending SyncStep1 so local edits made offline reach the server; a 4401-style
  refusal or HTTP 401 sends the user to `<prefix>/ui/login?next=<this page>`.
- Header status: Live (green dot) / Connecting / Reconnecting / Read-only (viewer).
- Sidebar: `model::catalogues`, smart ones marked, counts, the trash count, selection in the URL hash
  (`#c=<uuid>`).

- [ ] Native unit tests for the pure parts (`RoomConfig` parsing from a meta map, backoff sequence, catalogue list
  rows from a `Doc` built with `model` writes).
- [ ] Screenshot check: run a local server (`COCAT_WEB_DIR` = the trunk `dist`, a seeded room with 3 catalogues,
  one smart), log in with curl to get the cookie, open `/ui/rooms/<id>/browse` in headless Firefox with that
  cookie (a temporary profile's cookie store, or a one-time login page flow), screenshot 1440 and 390 px.
- [ ] Gate, commit "cocat-web: live replica and catalogue list".

### Task 4: Browse a catalogue

**Files:** `crates/cocat-web/src/rows.rs` (pure: rows, sort, filter, durations), `src/views/{timeline.rs,
table.rs,event_panel.rs,rule.rs}`

**Interfaces:**
- `fn rows(txn, catalogue_uuid) -> Result<Vec<Row>, RuleError>` (members via `model::catalogue_events`, derived
  intervals included with `uuid: None` and their sources); `Row { uuid, start, stop, tags, confidence, status,
  author, attributes, sources }`.
- `fn filter(rows, query: &str) -> Vec<&Row>` matches tags, status, author, attribute values, and time prefixes;
  `fn sort(rows, key: SortKey, descending)`.
- `fn duration_text(ns: i64) -> String` ("41 s", "1 min 03 s", "2 h 05 min", "3 d 4 h").
- Table: virtualised (only rows in view + 20 rendered; fixed row height), sortable headers, attribute columns
  togglable; timeline strip over the catalogue's time span with a span per event (two lanes alternating);
  event panel read-only in this task; smart catalogue header shows the rule in words and its error when invalid.
  The trash view lists trashed events.

- [ ] Native unit tests for `rows`, `filter`, `sort`, `duration_text` (values above), and an invalid rule giving
  `Err` with the core error text.
- [ ] Screenshot check as in Task 3 on a room with a 10k-event catalogue (generated through `model` in a small
  helper binary or test fixture), at 1440 and 390 px; note the scroll smoothness by scrolling in a scripted
  Firefox session if possible, else say it was not measured.
- [ ] Gate, commit "cocat-web: browse catalogues".

### Task 5: Edit

**Files:** `src/edit.rs` (pure: the write operations as functions on `TransactionMut` wrapping `model` writes),
`src/views/event_panel.rs`, `src/views/rule.rs`, `src/views/sidebar.rs`

- Event panel edits status, confidence, start/stop and their errors, tags, comment, attributes (add, change,
  remove); buttons: trash / restore / purge (purge confirms); add to / remove from catalogue; new event (current
  catalogue, now ±1 min by default); new catalogue; rule editor (JSON textarea, live validation message from
  `rules`, save only when valid); materialize (asks a name). Every write is one transaction.
- Viewers get none of these controls.

- [ ] Native unit tests for each `edit.rs` function (doc before/after through `model` reads).
- [ ] A two-replica test in native Rust: two `Doc`s exchanging updates, concurrent confidence edits converge.
- [ ] Screenshot check with an event selected and the rule editor showing an error.
- [ ] Gate, commit "cocat-web: edit events and catalogues".

### Task 6: Presence

**Files:** `src/presence.rs`, `src/views/header.rs`, `src/views/table.rs`

- Uses `yrs::sync::Awareness`; local state `{ "editing": <event uuid or null> }` (the server sets `user`);
  header avatars (initials, the server's colour, title = email); rows being edited by someone else get that
  colour's tint and left edge. Awareness frames go over the same websocket.

- [ ] Native unit test: avatars and row highlights derived from an awareness state map.
- [ ] Screenshot check with two browser sessions (two accounts) on the same room.
- [ ] Gate, commit "cocat-web: presence".

### Task 7: History

**Files:** `src/history.rs` (pure: version list grouping by day, summary text, diff rows), `src/api.rs` (fetch
helpers), `src/views/history.rs`

- A "History" button opens the history view (URL hash `#h=<version>`): the version list by day (Today,
  Yesterday, dates), author dot, summary, names; "Named only" filter; selecting shows the diff table (added /
  changed old → new / removed, "Only show changed events" on by default); banner with "Back to now" and "Restore
  this version" (editors; confirms; restores the catalogue on screen or, from the room-wide view, the room);
  "Name this version" (editors). The event panel's "Last changes" lists the latest versions touching that event
  with links.

- [ ] Native unit tests for grouping by day (with a fixed "now"), summary text ("2 added, 1 changed, 1 removed",
  "Rule edited", "Imported from tscat" from `auto_name`), and diff rows.
- [ ] Screenshot check of a room with a few versions by two accounts.
- [ ] Gate, commit "cocat-web: history".

### Task 8: Ship to cocat-dev

- [ ] Build the image, deploy cocat-dev, open the largest dev room in Firefox: record load time and the tab's
  memory (`about:memory` or `performance.memory` equivalent) in `docs/deploy.md`; if the room does not open in
  under 15 s or takes over 1.5 GB, note it in the report for the lead (the read-only fallback is a later plan).
- [ ] End-to-end: Python `cocat_rs` edits an event, the browser shows it live; the browser edits, Python sees it;
  the history lists both with the right authors.
