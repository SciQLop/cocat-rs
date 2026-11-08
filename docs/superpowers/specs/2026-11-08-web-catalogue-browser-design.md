# Web catalogue browser and version history

Status: design, 2026-11-08. Mockups: the "CoCat catalogue browser" canvas,
https://claude.ai/artifact/SuTuhNPhynjbt9VmCYsfiv (two boards, "Browser — live catalogue"
and "History — a past version, with its changes"; private to the owner).

## Why

Today the web pages only manage accounts, rooms, groups and invitations. To look at a
catalogue you need SciQLop or Python. People want to open a room in a browser, browse and
fix its catalogues with colleagues, and see what changed, by whom and when, the way Google
Docs shows a document's version history.

## Goals

1. **A live catalogue browser** in the browser, built with Leptos (Rust compiled to
   WebAssembly), holding its own replica of the room and syncing over the same websocket
   as SciQLop and Python. Edits from anyone appear without a reload.
2. **Browse:** the room's catalogues (static and smart, smart ones marked), the trash, a
   timeline strip of the selected catalogue's events, and an event table that sorts and
   filters (start, duration, tags, confidence, status, author; attributes as optional
   columns). Smart catalogues show their rule in words and their derived intervals.
3. **Edit:** an event side panel edits status, confidence, tags, comment, attributes and the
   range; create, trash, restore events; add or remove an event from a catalogue; create a
   catalogue; edit a smart catalogue's rule (a JSON editor with the rule error text is
   enough at first); materialize. Viewers (room role `viewer`) get the same pages
   read-only.
4. **Presence:** who is in the room now (avatars in the header), and the row each person is
   editing, in their colour. Uses the Yjs awareness protocol.
5. **Version history, Google Docs style:**
   - The list of versions, newest first, grouped by day. A version is a run of changes by one
     author with no gap longer than 5 minutes (builder may tune). Each shows a time, the
     author (a person, or the origin name for detector or import writes) and a one-line
     summary ("2 added, 1 changed, 1 removed", "Rule edited").
   - Picking a version shows the catalogue (or the whole room) as it was then, read-only,
     with its changes against the version before it: added events green, changed fields
     old → new, removed events struck through. A switch hides unchanged events. Comparing
     two arbitrary versions is a nice-to-have.
   - **Restore this version:** writes, as one new change by the person restoring, whatever
     makes the room equal to that version. History is never rewritten; the restore is
     itself a version and can be undone by restoring the one before it.
   - **Named versions:** a person can name a version ("tscat import", "paper v1"); a
     "Named only" filter. Imports and materializations name theirs automatically.
   - An event's side panel shows its last few changes with a link into the history.
6. The history is also available to Python (`room.history()`, `room.at(version)`), since
   it lives on the server (below).

## Non-goals

- Replacing the account, room, group and admin pages (askama, server-rendered): they stay.
  The browser is reached from a room's page and links back to it.
- Plotting data (SciQLop does that); the timeline strip only shows where events are.
- Offline editing in the browser beyond what the replica gives for a dropped connection.
- Attribution of changes made before history stamping ships: they show as
  "before history" with no author, still diffable.

## Anti-goals

- A second data path: the browser must not edit through REST. It writes the room like any
  client, so SciQLop, Python and the browser always agree.
- Losing changes on restore, or a restore that only one client sees.
- A page that freezes on a room of 100k events: the table must virtualise its rows, and
  smart catalogues evaluate as they do in Python (about 70 ms per read at 100k events).

## Constraints

- Same origin as the server, under the proxy prefix (`/cocat-dev/...`); authentication is
  the existing session cookie; the websocket needs the `cocat.v2` subprotocol.
- Room layout v2 is the data model; reading and writing go through `cocat-core::model` and
  `cocat-core::rules`, compiled to wasm, not reimplemented in the browser.
- Look: the existing stylesheet's tokens (ink, muted, accent blue `#2753b8`, amber
  `#f0ad2e`, Inter or the system font, monospace for times), the CoCat logo, and it works at
  phone width (the side panels stack).
- Repo rules hold: TDD, the full gate before commits, no machine paths committed.

## Decided

- **Leptos, client-side rendered**, built to a static wasm bundle served by `cocat-server`
  under `/ui/app/` (my call: no server-side rendering, so the server stays one binary with
  embedded assets and the browser holds the replica).
- **History lives on the server** (my call): the server already sees every update and who
  sent it, and keeps the whole update log (compaction was dropped).
  - Each update the server appends gets a stamp: user id, time, and the origin id when the
    update carries one. Stored in an append-only side file next to the room file, one
    entry per record, so the `.y` format does not change (my call; a SQLite table is the
    alternative if the side file proves awkward).
  - A version at record N is the room replayed up to N; diffs compare the model's reads
    (catalogues, events by uuid, fields, members) at two points.
  - API: list versions (grouped as above), the room or one catalogue at a version, the diff
    between two versions, name a version, restore a version. Restore builds the update on
    the server and broadcasts it as a change by the restoring user.
  - Named versions are stored in the server database.

## Settled after the builder check (my calls, the user was away)

1. **Author and origin.** No protocol change. The author of a record is the account that
   sent it; the server knows it. A detector or import shows as "alexis.jeandet via
   sw-finder 1.2": the history reads the record's diff, and new events carrying an `origin`
   (or a new entry in the `origins` root) name it. A record that adds an `origins` entry of
   kind `import` or `rule` (materialize) names its version after that origin automatically.
   All of this is computed when the history is read, not when the update is written.
2. **Scope.** One version list per room; the browser filters it to the catalogue on
   screen (versions that touched its fields, its members, or its members' events). Restore
   acts on what is on screen: the whole room, or one catalogue. Restoring one catalogue sets
   its fields, rule and members back, and sets its member events' fields back (they are
   the same events everywhere; that is expected). Events created after the version are
   moved to the trash, never purged, so a restore is undoable.
3. **Where history is collected.** Wherever this server runs: cocat-dev now, production
   when it switches (a separate decision for the user). Each stamp also holds the record's
   length and a 64-bit hash of its bytes; on load, stamps that no longer match their
   record (a re-seed swapped the file) are dropped and those records read as "before
   history". A side file stays the store; it moves and dies with the room file.
4. **Presence.** The server relays the Yjs awareness protocol between clients and
   overwrites each state's `user` field (email, colour derived from it) with the
   authenticated account, so nobody can pose as someone else. The browser publishes the
   event it is editing. cocat-client and `cocat_rs` learn awareness later, as a separate
   change; until then SciQLop and Python users are not shown.
5. **Big rooms.** The browser holds the full replica. Part of the done check is opening the
   largest cocat-dev room in Firefox and recording memory and load time. If the replica is
   too heavy, rooms above a measured size open read-only through server-side reads (the
   history API already gives the room at a version); that fallback is only built if the
   measurement asks for it.
6. **Python.** `room.history()` returns the version list; `room.at(version)` returns a
   read-only `Room` replayed from the update records the server sends up to that version.

## Left to the builder

- Crate layout (a `cocat-web` crate is the obvious one), the build tool (`trunk` or
  `cargo-leptos`), how the bundle is embedded and fingerprinted, CI for the wasm target.
- Making `cocat-core` build for `wasm32` (the file store behind a feature, uuid's `js`
  feature) and `cocat-client`'s sync loop on the browser's WebSocket.
- Caching replayed versions so the history list stays fast on big rooms (the 0.7 s replay of
  the largest production room is the budget to beat).
- The table component and its virtualisation, the timeline strip's zoom.
- The exact summary wording and version-grouping gap.
- Order of delivery: the browser first (read, then edit, then presence), the history
  stamping as soon as possible since every day without it is history without authors,
  then the history UI.

## Done means

On cocat-dev, two people edit the same catalogue, one in SciQLop and one in the browser,
and each sees the other's edits live; the history lists their versions with names and
summaries; restoring an earlier version brings the catalogue back for both; Python reads
the same history.

## Later: copies and "Apply" (decided 2026-11-08)

People never see developer words. On screen: "Save version" (with a name and a note),
"Restore this version", "What changed", "Make a copy from this version" (a new project equal
to a past version, same uuids), and, not built yet, **"Apply to <original project name>"**
to bring a copy's changes back. Apply opens a preview first: what it adds, changes and
removes, and the events changed on both sides, where the person picks which value to keep.
Only then an "Apply" button writes. Build it when someone actually works in a copy and
wants their changes back.
