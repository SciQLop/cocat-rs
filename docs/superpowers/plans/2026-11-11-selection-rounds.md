# Selection Rounds Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A project can run selection rounds: named people propose interval lists in their own documents, vote, and a decider records one final catalogue, with a telemetry budget computed the same way by every client.

**Architecture:** Rounds, their roles, proposals, votes and log live in four SQLite tables. Each proposal is a per-document room `{project}/{doc}` (the existing document socket and history), registered in `round_proposal` instead of `document`. One server function decides the role a user has on any room key; the socket, both role re-checks and the document-aware HTTP routes ask it, so freezing or hiding a proposal is a role change the existing 30 s re-check enforces. Budget parsing and cost live in `cocat-core`, used by the server, the web page and Python.

**Tech Stack:** Rust (axum, sqlx/SQLite, yrs, utoipa, leptos for cocat-web, pyo3 for cocat-py), `winnow` (already a cocat-core dependency) for the unit parser, `time` for deadlines.

**Spec:** `docs/superpowers/specs/2026-11-11-selection-rounds-design.md`

## Global Constraints

- Round states, in order: `proposing`, `voting`, `decided`. Visibility: `open`, `blind`. Round roles: `proposer`, `voter`, `decider`.
- A decider moves forward (and may skip `voting`); only a project **owner** moves back. Every state change is logged with who did it (`NULL` actor = the deadline).
- Owners and editors create rounds. A decider must be an owner or editor; proposers and voters may be viewers. At creation, every current owner and editor is proposer and voter; the creator is decider.
- Blind proposal readers before the round leaves `proposing`: its author, the round's deciders, the project's owners. Votes in a blind round: the voter, deciders and owners until `decided`. Open rounds: every project member reads everything.
- Writes: a proposal only by its author, only while `proposing` and while the author is a proposer; votes only by the voter, only while `voting`.
- Budget is a guide: never refuse a decision for being over the cap. Unpriced intervals are listed, never counted as zero silently.
- Units: VOUnits spellings only (`Mibyte`, `kbit`, `byte/s`…; `MiB` refused). Data: `bit`, `byte`, decimal prefixes `k M G T`, binary prefixes `Ki Mi Gi Ti`. Time: `s`, `min`, `h`, `d`. A rate is data or data/time; a cap is data. Zero-length interval costs zero.
- Final events carry `proposed_by` (list of user emails) and `votes` (integer); the final catalogue carries a `round` attribute holding the round's JSON-LD description as text.
- No changes to SciQLop or the SciQLop plugin.
- One build or test command at a time, in the foreground. Full gate before each commit: `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`, `cargo clippy -p cocat-web --target wasm32-unknown-unknown -- -D warnings`; Python tasks also `cargo build --workspace && maturin develop -m crates/cocat-py/Cargo.toml && python -m pytest crates/cocat-py/tests`.
- Commit straight to `main` with explicit paths; never push. No machine-local paths in the repo.

## Review Focus

1. A blind proposal leaking through a route other than the socket: `GET …/history?document=`, `GET …/history/{v}/state?document=`, `POST …/history/{v}/restore` with `document`, `GET …/documents` — each must answer 404 to a non-reader (Task 4 test `a_blind_proposal_is_invisible_to_another_proposer_on_every_route`).
2. An author's open socket after the round moves to `voting`: it must be closed by the re-check and reopen read-only, so a late offline edit is dropped (Task 4 test `freezing_closes_the_author_socket_and_reopens_it_read_only`).
3. A viewer named proposer: they must be able to write their own proposal without the 30 s re-check closing their socket every period (Task 4 test `a_viewer_proposer_keeps_a_writable_socket_across_rechecks`).
4. An owner moving a decided round back to `proposing`: proposals become writable again and the move is logged (Task 3 test `an_owner_moves_a_round_back_and_it_is_logged`).
5. A budget whose rate is in `kbyte/s` and cap in `Mibyte`: converted, not refused; `MiB` refused with a reason (Task 1 tests).

---

### Task 1: Units, budget and cost in cocat-core

**Files:**
- Create: `crates/cocat-core/src/round/mod.rs`, `crates/cocat-core/src/round/units.rs`, `crates/cocat-core/src/round/budget.rs`
- Modify: `crates/cocat-core/src/lib.rs` (add `pub mod round;`)

**Interfaces:**
- Produces:
  - `pub struct Unit { pub bits: f64, pub per_seconds: Option<f64> }` — bits per one unit; `per_seconds` is the time divisor in seconds when the unit is a rate.
  - `pub fn parse_unit(notation: &str) -> Result<Unit, UnitError>`
  - `pub struct Quantity { pub value: f64, pub notation: String, pub unit: Unit }`
  - `pub struct Budget { pub attribute: String, pub cap: Quantity, pub rates: Vec<(String, Quantity)> }`
  - `pub fn parse_budget(json: &serde_json::Value) -> Result<Budget, BudgetError>` — reads exactly the compact JSON-LD shape of the spec (`cocat:attribute`, `cocat:cap`, `cocat:rates[] {cocat:value, cocat:rate}`, `qudt:numericValue`, `qudt:hasUnit.skos:notation`); `BudgetError` messages name the offending field.
  - `pub fn budget_json(budget: &Budget) -> serde_json::Value` — writes the same shape back, with the spec's `@context` and the VOUnits `dcterms:conformsTo` on each unit.
  - `pub struct Cost { pub total: f64, pub cap: f64, pub notation: String, pub unpriced: Vec<String> }` — `total` and `cap` in the cap's unit; `unpriced` holds event uuids.
  - `pub fn cost(events: &[EventRecord], budget: &Budget) -> Cost`
  - `Cost::text(&self) -> String` → `"320 of 500 Mibyte used"`, plus `", 3 intervals without a rate"` when any (`"1 interval without a rate"` for one).

- [ ] **Step 1: Write the failing tests** (in `units.rs` and `budget.rs`, `#[cfg(test)]`)

```rust
#[test] fn data_units_follow_vounits() {
    assert_eq!(parse_unit("byte").unwrap().bits, 8.0);
    assert_eq!(parse_unit("kbit").unwrap().bits, 1000.0);
    assert_eq!(parse_unit("Mibyte").unwrap().bits, 8.0 * 1048576.0);
    assert!(parse_unit("MiB").is_err());
    assert!(parse_unit("Mibyte/fortnight").is_err());
}
#[test] fn a_rate_divides_by_a_time() {
    assert_eq!(parse_unit("Mibyte/min").unwrap().per_seconds, Some(60.0));
    assert_eq!(parse_unit("kbyte/s").unwrap().per_seconds, Some(1.0));
    assert_eq!(parse_unit("byte").unwrap().per_seconds, None);
}
#[test] fn the_spec_budget_parses_and_round_trips() // spec JSON → parse_budget → budget_json → parse_budget equal
#[test] fn a_cap_that_is_a_rate_is_refused() // cap "Mibyte/min" → Err naming "cocat:cap"
#[test] fn cost_is_rate_times_duration_in_the_cap_unit() {
    // budget burst 2 Mibyte/min, normal 0.1 Mibyte/min, cap 500 Mibyte
    // events: burst 10 min, normal 60 min, burst 0 s → total 26.0 (20 + 6 + 0)
}
#[test] fn rates_in_other_units_convert() // rate 1024 "kibyte/s", one 60 s event, cap in "Mibyte" → total 60.0
#[test] fn a_rate_without_time_is_a_fixed_cost_per_interval() // rate "Mibyte" 5, two events → 10.0
#[test] fn events_without_a_rate_are_listed_not_counted() {
    // one event with no `mode`, one with mode "survey" not in the table → unpriced has both uuids, total 0.0
    // text(): "0 of 500 Mibyte used, 2 intervals without a rate"
}
```

- [ ] **Step 2:** `cargo test -p cocat-core round::` — FAIL (module missing).
- [ ] **Step 3: Implement.** Parse units with `winnow`: `<prefix?><bit|byte>` optionally `/` `<s|min|h|d>`. Event duration from `stop - start` in seconds (Timestamp is ns). Attribute lookup: `event.attributes.get(&budget.attribute)` as a string. Add `// simplify: a VOUnits subset (data and time only); a full VOUnits/QUDT parser when budgets need other quantities.` and `// simplify: reads one compact JSON-LD shape, no JSON-LD expansion; add a JSON-LD processor if other tools write budgets differently.`
- [ ] **Step 4:** `cargo test -p cocat-core round::` — PASS; then the full gate.
- [ ] **Step 5: Commit** — `git add crates/cocat-core/src/round crates/cocat-core/src/lib.rs && git commit -m "Round budgets: VOUnits subset and cost" -- crates/cocat-core/src/round crates/cocat-core/src/lib.rs`

### Task 2: Round tables and the access rules

**Files:**
- Create: `crates/cocat-server/migrations/0024_rounds.sql`, `crates/cocat-server/src/rounds.rs` (DB rows, queries, and the pure access rules in an inner `pub mod rules`)
- Modify: `crates/cocat-server/src/lib.rs` (`pub mod rounds;`)

**Interfaces:**
- Produces:
  - Tables:
    - `round(id TEXT PK, room_id TEXT NOT NULL REFERENCES room(id) ON DELETE CASCADE, name TEXT NOT NULL, state TEXT NOT NULL CHECK (state IN ('proposing','voting','decided')) DEFAULT 'proposing', visibility TEXT NOT NULL CHECK (visibility IN ('open','blind')), deadline TEXT, budget TEXT, final_catalogue TEXT, created_by TEXT REFERENCES "user"(id) ON DELETE SET NULL, created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP)`
    - `round_member(round_id REFERENCES round(id) ON DELETE CASCADE, user_id REFERENCES "user"(id) ON DELETE CASCADE, role TEXT CHECK (role IN ('proposer','voter','decider')), PRIMARY KEY (round_id, user_id, role))`
    - `round_proposal(round_id … CASCADE, author TEXT NOT NULL REFERENCES "user"(id) ON DELETE CASCADE, doc_id TEXT NOT NULL UNIQUE, created_at …, PRIMARY KEY (round_id, author))`
    - `round_vote(round_id … CASCADE, voter … CASCADE, event TEXT NOT NULL, PRIMARY KEY (round_id, voter, event))`
    - `round_log(round_id … CASCADE, at DATETIME DEFAULT CURRENT_TIMESTAMP, actor TEXT REFERENCES "user"(id) ON DELETE SET NULL, what TEXT NOT NULL)`
  - `#[derive(sqlx::Type, Serialize, Deserialize, utoipa::ToSchema)] pub enum RoundState { Proposing, Voting, Decided }` (lowercase, `Ord` in that order), `pub enum Visibility { Open, Blind }`, `pub enum RoundRole { Proposer, Voter, Decider }`.
  - `pub struct RoundRow { id: Uuid, room_id: String, name: String, state: RoundState, visibility: Visibility, deadline: Option<String>, budget: Option<String>, final_catalogue: Option<String>, created_by: Option<Uuid>, created_at: String }`
  - Queries (all `pool: &SqlitePool`, `Result<_, DbError>`): `create_round(pool, room_id, name, visibility, deadline, budget, by) -> Uuid` (also inserts default members, in one transaction), `round(pool, id) -> Option<RoundRow>`, `rounds_of(pool, room_id) -> Vec<RoundRow>`, `update_round(pool, id, RoundPatch)`, `set_state(pool, id, RoundState, final_catalogue: Option<&str>, actor: Option<Uuid>)` (writes the log line `"state: voting"` etc.), `members(pool, id) -> Vec<(Uuid, RoundRole)>`, `set_members(pool, id, &[(Uuid, RoundRole)])`, `proposal_of(pool, id, author) -> Option<Uuid>`, `add_proposal(pool, id, author) -> Uuid`, `proposals(pool, id) -> Vec<(Uuid /*author*/, Uuid /*doc*/)>`, `proposal_by_doc(pool, room_id, doc) -> Option<(RoundRow, Uuid /*author*/)>`, `set_votes(pool, id, voter, &[String])`, `votes(pool, id) -> Vec<(Uuid, String)>`, `log(pool, id) -> Vec<LogLine>`, `due_rounds(pool, now: &str) -> Vec<Uuid>` (proposing with `deadline <= now`).
  - `pub mod rules`:
    - `pub struct Seat { pub project_role: Role, pub roles: Vec<RoundRole> }`
    - `pub fn proposal_role(round: &RoundRow, seat: &Seat, is_author: bool) -> Option<Role>` — `Some(Editor)` iff author ∧ proposer ∧ proposing; `Some(Viewer)` iff readable; else `None`.
    - `pub fn may_read_votes(round: &RoundRow, seat: &Seat, is_voter_self: bool) -> bool`
    - `pub fn may_move(round: &RoundRow, seat: &Seat, to: RoundState) -> bool` — forward: decider; backward: project owner.

- [ ] **Step 1: Write the failing tests** (`rounds.rs` `#[cfg(test)]` for `rules`, `crates/cocat-server/tests/rounds_db.rs` for queries)

```rust
#[test] fn the_author_writes_only_while_proposing()
#[test] fn a_removed_proposer_reads_but_no_longer_writes()
#[test] fn a_blind_proposal_is_read_by_author_deciders_and_owners_only() // editor non-decider → None; owner → Viewer
#[test] fn every_member_reads_a_blind_proposal_once_proposing_ends()
#[test] fn an_open_proposal_is_read_by_every_member()
#[test] fn blind_votes_are_hidden_until_decided()
#[test] fn deciders_move_forward_and_owners_move_back() // decider-editor: proposing→decided true, voting→proposing false; owner: decided→proposing true
#[tokio::test] async fn a_new_round_seats_owners_and_editors_and_its_creator_decides() // viewers get no seat
#[tokio::test] async fn deleting_the_project_deletes_its_rounds()
#[tokio::test] async fn due_rounds_are_the_proposing_ones_past_their_deadline()
```

- [ ] **Step 2:** `cargo test -p cocat-server rounds` — FAIL.
- [ ] **Step 3: Implement** the migration, queries and rules.
- [ ] **Step 4:** tests PASS; full gate.
- [ ] **Step 5: Commit** (`migrations/0024_rounds.sql`, `src/rounds.rs`, `src/lib.rs`, `tests/rounds_db.rs`).

### Task 3: Rounds API

**Files:**
- Create: `crates/cocat-server/src/rounds_api.rs`, `crates/cocat-server/tests/rounds_api.rs`
- Modify: `crates/cocat-server/src/api.rs` (merge `rounds_api::router()`), `crates/cocat-server/tests/access_matrix.rs` (rows, `{round}` placeholder and fixture, bodies)

**Interfaces:**
- Consumes: Task 2 queries and rules; Task 1 `parse_budget`, `budget_json`; `api::membership`, `lifecycle::require_role`, `ws::check_room_roles(state, project)`.
- Produces routes (JSON, `ApiError` as elsewhere; unknown round or non-member → 404; not allowed → 403 with a sentence):
  - `GET /api/rooms/{room_id}/rounds` → `Vec<RoundOut>` (Viewer)
  - `POST /api/rooms/{room_id}/rounds` body `{name, visibility, deadline?, budget?}` → `RoundOut` (Editor; refused on public and personal projects like `add_document`; an invalid budget → 422 with the `BudgetError` text)
  - `GET /api/rooms/{room_id}/rounds/{round}` → `RoundOut`
  - `PATCH /api/rooms/{room_id}/rounds/{round}` body `{name?, visibility?, deadline?, budget?}` (decider or owner; refused once `decided`)
  - `PUT /api/rooms/{room_id}/rounds/{round}/members` body `{proposers: [email], voters: [email], deciders: [email]}` (decider or owner; every email must be a project member; a decider must be owner/editor → 422)
  - `POST /api/rooms/{room_id}/rounds/{round}/state` body `{state, final_catalogue?}` (`may_move`; `decided` needs `final_catalogue` naming a catalogue of the project's catalogues document → 422 otherwise)
  - `RoundOut { id, project, name, state, visibility, deadline, budget: Option<Value>, final_catalogue, proposers: [email], voters: [email], deciders: [email], proposals: [{author: email, doc: Option<Uuid>}], log: [{at, actor: Option<email>, what}] }` — `doc` is `None` when the caller may not read that proposal (Task 2 rules).
  - After any change of state, visibility or members: `ws::check_room_roles(state, project).await`.

- [ ] **Step 1: Write the failing tests** in `tests/rounds_api.rs` (copy the `Fixture` pattern of `tests/workspaces.rs:31-128`):

```rust
async fn an_editor_creates_a_round_and_a_viewer_cannot()
async fn a_bad_budget_is_refused_with_its_reason() // cap "MiB" → 422, body names "MiB"
async fn the_budget_comes_back_as_json_ld()
async fn a_decider_must_be_an_owner_or_editor()
async fn deciding_needs_an_existing_catalogue()
async fn an_owner_moves_a_round_back_and_it_is_logged() // decided → proposing; log has "state: proposing" by the owner's email
async fn an_editor_decider_cannot_move_back()
async fn a_blind_round_hides_who_wrote_which_document() // another proposer sees proposals with doc: None
```

- [ ] **Step 2:** FAIL. **Step 3:** implement; add access-matrix rows (`Guard::RoomViewer` for GETs, `Guard::RoomEditor` for POST rounds, others `RoomViewer` + `.tolerant()` since the round rules refine them). **Step 4:** `cargo test -p cocat-server --test rounds_api --test access_matrix` PASS; full gate. **Step 5: Commit.**

### Task 4: Proposal documents and one access function for every room key

**Files:**
- Create: `crates/cocat-server/src/access.rs`, `crates/cocat-server/tests/proposals.rs`
- Modify: `crates/cocat-server/src/ws.rs` (`lobby_role`, `room_to_relay`, `recheck`, `recheck_after_attach`), `crates/cocat-server/src/history_api.rs` (`history_key`), `crates/cocat-server/src/documents.rs` (`list_documents`), `crates/cocat-server/src/rounds_api.rs` (new route)

**Interfaces:**
- Consumes: Task 2 `proposal_by_doc`, `members`, `rules::proposal_role`.
- Produces:
  - `pub async fn key_role(state: &AppState, key: &str, user: Uuid) -> Result<Option<Role>, DbError>` in `access.rs`: project role for the catalogues key and for `document` rows; `rules::proposal_role` for a proposal doc; `None` for anything else.
  - `POST /api/rooms/{room_id}/rounds/{round}/proposal` → `{doc: Uuid}`: the caller's proposal, created on first call (caller must be a proposer and the round `proposing`, else 403).
  - `ws::room_to_relay` accepts a doc that is a proposal; the socket's role is `key_role` of the key (not the project role); `recheck` and `recheck_after_attach` compare against `key_role(state, room_key, user)`.
  - `history_key` and `list_documents` treat a proposal the caller may not read as missing (404 / left out); `list_documents` lists readable proposals with kind `"proposal"` (add `Proposal` to the JSON enum only, not to the `document` table).

- [ ] **Step 1: Write the failing tests** in `tests/proposals.rs`:

```rust
async fn a_proposer_opens_and_writes_their_proposal_socket()
async fn a_viewer_proposer_keeps_a_writable_socket_across_rechecks() // call ws::check_room_roles twice; socket stays open, ROLE_HEADER "editor"
async fn a_blind_proposal_is_invisible_to_another_proposer_on_every_route()
    // socket 403; history?document= 404; history/0/state?document= 404; restore 404; documents list omits it
async fn a_decider_and_an_owner_read_a_blind_proposal()
async fn freezing_closes_the_author_socket_and_reopens_it_read_only()
    // state → voting; author socket closed with 4403; reconnect ROLE_HEADER "viewer"; an update sent then is not stored
async fn opening_the_round_to_all_shows_proposals_to_every_member()
```

- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** `cargo test -p cocat-server --test proposals --test workspaces --test access_matrix` PASS; full gate. **Step 5: Commit.**

### Task 5: Votes

**Files:**
- Modify: `crates/cocat-server/src/rounds_api.rs`, `crates/cocat-server/tests/rounds_api.rs`, `crates/cocat-server/tests/access_matrix.rs`

**Interfaces:**
- Produces:
  - `PUT /api/rooms/{room_id}/rounds/{round}/votes` body `{events: [uuid]}` — replaces the caller's votes (voter and `voting`, else 403).
  - `GET /api/rooms/{room_id}/rounds/{round}/votes` → `[{voter: email, events: [uuid]}]`, only the votes the caller may read (`rules::may_read_votes`).

- [ ] **Step 1: Failing tests:** `a_voter_replaces_their_votes`, `votes_are_refused_outside_voting`, `blind_votes_show_only_to_the_voter_and_deciders`, `open_votes_show_to_every_member`.
- [ ] **Steps 2–5** as above (`--test rounds_api --test access_matrix`).

### Task 6: Deadlines

**Files:**
- Modify: `crates/cocat-server/src/rounds.rs` (`pub fn spawn_deadlines(state: AppState) -> JoinHandle<()>`, `pub async fn close_due(state: &AppState, now: OffsetDateTime) -> Result<usize, DbError>`), `crates/cocat-server/src/cli.rs` (spawn next to `ws::spawn_role_checks`)

**Interfaces:**
- `close_due` moves every due round to `voting` with a `NULL` actor (log `"state: voting"`), runs `check_room_roles` for its project, returns how many it moved. The loop wakes every 60 s like `relay::schedule::spawn_scheduler`.

- [ ] **Step 1: Failing tests** (`tests/rounds_db.rs`): `a_past_deadline_moves_the_round_to_voting_by_itself` (log actor `None`), `a_future_deadline_does_nothing`, `a_round_without_deadline_is_never_moved`.
- [ ] **Steps 2–5** as above.

### Task 7: Deciding records the round on the final catalogue

**Files:**
- Modify: `crates/cocat-server/src/rounds_api.rs`, `crates/cocat-server/tests/rounds_api.rs`

**Interfaces:**
- On a move to `decided`, the server writes, through `state.rooms.write_as` with the decider as author, the catalogue attribute `round` = JSON text `{"@context": …, "@type": "cocat:Round", "cocat:name", "cocat:decidedBy": email, "cocat:decidedAt", "cocat:proposers": [email], "cocat:budget": <budget_json or absent>}`, using `model::set_catalogue_attributes`.
- `proposed_by` / `votes` on events are written by the decider's client (trusted, per spec); the server does not check them.

- [ ] **Step 1: Failing test:** `deciding_stamps_the_round_on_the_final_catalogue` (read the catalogue via `GET …/catalogues/{id}`; its `round` attribute parses and names the decider and the budget's cap).
- [ ] **Steps 2–5** as above.

### Task 8: Python client and bindings

**Files:**
- Modify: `crates/cocat-client/src/room.rs` (`websocket_url`: push each `/`-separated segment of `room_id`), `crates/cocat-client/src/lib.rs`
- Create: `crates/cocat-client/src/rounds.rs`, `crates/cocat-py/src/rounds.rs`, `crates/cocat-py/tests/test_rounds.py`
- Modify: `crates/cocat-py/src/lib.rs` (register), `crates/cocat-py/python/cocat_rs/client.py`, `crates/cocat-py/python/cocat_rs/__init__.py`, `crates/cocat-py/python/cocat_rs/raw.py`

**Interfaces:**
- `cocat-client` free functions `(url, cookie, project, …) -> Result<Value, LoginError>` for every Task 3/4/5 route: `rounds`, `create_round`, `round`, `update_round`, `set_round_members`, `move_round`, `my_proposal`, `set_votes`, `votes`.
- Python, on `Project`: `rounds()`, `create_round(name, visibility="open", deadline=None, budget=None)`, `round(id)`, `update_round(id, **fields)`, `set_round_members(id, proposers, voters, deciders)`, `move_round(id, state, final_catalogue=None)`, `my_proposal(id) -> Project` (opens `f"{project}/{doc}"` through the existing `open` path), `vote(id, events)`, `votes(id)`. Module function `cocat_rs.round_cost(events, budget) -> dict` with `total`, `cap`, `unit`, `unpriced`, `text`, over `Event` objects or dicts.

- [ ] **Step 1: Failing tests** in `test_rounds.py`: `test_a_round_from_proposal_to_decision` (two accounts: each writes intervals with `mode` into `my_proposal`, decider moves to voting, votes, writes the final catalogue with `proposed_by`, moves to decided; the final catalogue has `round`), `test_a_blind_proposal_cannot_be_opened_by_another_proposer` (raises `Forbidden`), `test_round_cost_matches_the_server_budget`; plus a Rust test in `cocat-client` `a_document_room_url_keeps_its_slash_as_a_path_segment`.
- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** full gate including the Python suite. **Step 5: Commit.**

### Task 9: Rounds on the web page (read-only)

**Files:**
- Create: `crates/cocat-web/src/views/rounds.rs`, `crates/cocat-web/src/rounds.rs` (JSON shapes and pure text helpers)
- Modify: `crates/cocat-web/src/route.rs` (`Route::Rounds`, `Route::Round(String)`, hash `#/rounds`, `#/rounds/{id}`), `crates/cocat-web/src/app.rs`, `crates/cocat-web/src/views/mod.rs`, `crates/cocat-web/src/views/sidebar.rs` (a "Rounds" entry when the project has any), `crates/cocat-web/style.css`

**Interfaces:**
- The round page shows: name, state, visibility, deadline; proposers with "submitted" or "not yet"; deciders; the log; once decided, a link to the final catalogue and `Cost::text()` of its events against the round's budget (computed with Task 1 `cost` over the replica's events), the total in red when over the cap.

- [ ] **Step 1: Failing tests** (pure, in `rounds.rs`): `the_route_round_trips_through_the_hash`, `a_proposer_line_says_whether_they_submitted`, `the_budget_line_turns_red_over_the_cap` (a `fn over_cap(&Cost) -> bool`).
- [ ] **Step 2–4:** FAIL, implement, gate (native + wasm clippy, `trunk build --release`); look at the page in desktop Firefox with a seeded round and screenshot it.
- [ ] **Step 5: Commit.** → skipped: creating and editing rounds from the web page; add when someone needs it without Python.
