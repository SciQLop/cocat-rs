# Rounds on the Web Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Regular users understand rounds and do their part from the web page; votes become per-event verdicts; review rounds start from a catalogue (or an imported file); rounds are shaped for later review cycles.

**Architecture:** The round's allowed moves become a per-kind data table read by the rules; moves carry notes and bump a revision. A `round_verdict` table replaces `round_vote`. A server-side copy fills a frozen *candidates* proposal. The web app gains a self-explaining rounds list, a "Your part" box, a new-round form with presets, a proposal editor (the catalogue view on the proposal's room), a keyboard-driven review table, and a decide view that writes the final catalogue under a deterministic uuid.

**Tech Stack:** Rust (axum, sqlx/SQLite, yrs, utoipa, leptos, pyo3), existing copy-chain and import code, `uuid` v5.

**Spec:** `docs/superpowers/specs/2026-11-11-rounds-web-workflow-design.md` (builds on `docs/superpowers/specs/2026-11-11-selection-rounds-design.md`, implemented).

## Global Constraints

- Kinds: `review`, `downlink`, `labelling`. Kind never changes rules except through its moves table. Existing rounds migrate to `downlink` when they have a budget, else `labelling`. `kind` is PATCHable; any kind may carry a budget.
- Moves tables (who ∈ `decider`, `owner`, `proposer`, `deadline`). For all three kinds today: `proposing→voting` decider|deadline, `proposing→decided` decider, `voting→decided` decider, any backward move owner. No other code may encode state order.
- A move may carry a `note` (free text, ≤ 2000 chars), stored in `round_log.note`, shown with the move. Each move into `proposing` from a later state increments `round.revision` (starts at 1).
- Verdicts: `accept | reject | unsure` per (round, reviewer, event), with the revision it was given in; absent = not reviewed. Written by voters only while `voting`. Read rules = the existing vote rules (blind: reviewer, deciders, owners until decided). Existing votes migrate to `accept` in revision 1. `vote(events)` = set `accept` on those, clear the voter's other `accept`s, keep `reject`/`unsure`. Bulk PUT merges.
- Progress counts only current-revision verdicts on current candidates; verdicts on events no longer among the candidates are "set aside" and counted separately.
- Candidates: copied at creation from a catalogue the creator can read (any project, public included), trash excluded, **event uuids kept**, copy hop recorded; stored in a proposal row with `candidates = 1`, author = creator; nobody writes it; readable by deciders and owners always, by every project member once the round leaves `proposing`. A review round with no proposers starts in `voting` (logged by its creator).
- Final catalogue from the web: uuid = uuid v5 (namespace URL, `"cocat:round:" + round_id`); events get fresh uuids and carry `candidate` (candidate uuid), `proposed_by` (list of emails), `accepts`, `rejects`, `unsure` (integers). Re-deciding rewrites the same catalogue.
- Wording: in review rounds the voter role shows as "reviewer" and `voting` shows as "reviewing". Intro sentence on the Rounds page, verbatim: "In a round, several people propose or review time intervals, then a decider keeps one final catalogue."
- Review keys: `a` accept, `r` reject, `u` unsure, `j` next, `k` previous; after a verdict the next unreviewed row is selected. Counter text: "{done} of {total} reviewed".
- No changes to SciQLop or its plugin. One build/test at a time, foreground. Full gate before each commit (fmt, clippy native, `cargo test --workspace`, clippy wasm; Python tasks add the pytest suite; web tasks add `trunk build --release`). Commit to `main` with explicit paths; never push.

## Review Focus

1. A blind round's verdicts leaking through `GET …/verdicts` to another reviewer before `decided` (Task 2 test `blind_verdicts_show_only_to_the_reviewer_deciders_and_owners`).
2. A pipeline's catalogue edited after the round started: candidates must not change (Task 3 test `candidates_are_a_snapshot`).
3. A reviewer who cannot read the source project still reads the candidates once reviewing starts; a creator who cannot read the source is refused (Task 3 tests).
4. Deciding twice from the page (retry after a failure) leaves one final catalogue, not two (Task 8 test `deciding_again_rewrites_the_same_catalogue`).
5. A move back to `proposing` then forward: old verdicts show as previous-revision, progress restarts at 0 (Task 2 test `a_new_revision_restarts_progress`).

---

### Task 1: Kinds, moves as data, notes, revisions

**Files:** Create `crates/cocat-server/migrations/0025_round_kinds.sql`. Modify `crates/cocat-server/src/rounds.rs`, `rounds_api.rs`, `tests/rounds_db.rs`, `tests/rounds_api.rs`, `tests/access_matrix.rs` (bodies).

**Interfaces — produces:**
- Migration: `round.kind TEXT NOT NULL DEFAULT 'labelling' CHECK (kind IN ('review','downlink','labelling'))` (backfill `downlink` where `budget IS NOT NULL`), `round.revision INTEGER NOT NULL DEFAULT 1`, `round_log.note TEXT`.
- `pub enum Kind { Review, Downlink, Labelling }` (text enum like the others); `RoundRow.kind`, `RoundRow.revision`; `LogLine.note: Option<String>`.
- `pub enum Mover { Decider, Owner, Proposer, Deadline }`; `pub fn moves(kind: Kind) -> &'static [(RoundState, RoundState, Mover)]`; `rules::may_move(round, seat, to)` and the deadline close both consult `moves` (deadline close asks for a `Deadline` row).
- `set_state(pool, id, from, to, final_catalogue, actor, note: Option<&str>) -> bool` increments `revision` when `to == Proposing && from != Proposing`.
- API: create and PATCH accept `kind`; `POST …/state` accepts `note` (> 2000 chars → 422); `RoundOut` gains `kind`, `revision`, `log[].note`.

- [ ] **Step 1: Failing tests:** `the_moves_table_reproduces_todays_rules` (every (from,to) × seat combination of today's `may_move` matrix gives the same answer), `a_note_travels_with_the_move`, `moving_back_starts_a_new_revision`, `existing_rounds_get_a_kind` (migration backfill), `a_kind_is_set_at_creation_and_changed_by_patch`, `a_too_long_note_is_refused`.
- [ ] **Step 2:** run, see FAIL. **Step 3:** implement. **Step 4:** gate. **Step 5:** commit.

### Task 2: Verdicts

**Files:** Create `migrations/0026_round_verdicts.sql`. Modify `rounds.rs`, `rounds_api.rs`, `tests/rounds_api.rs`, `tests/rounds_db.rs`, `tests/access_matrix.rs`.

**Interfaces — consumes:** Task 1 `RoundRow.revision`. **Produces:**
- Table `round_verdict(round_id … CASCADE, reviewer TEXT NOT NULL, event TEXT NOT NULL, verdict TEXT NOT NULL CHECK (verdict IN ('accept','reject','unsure')), revision INTEGER NOT NULL, at DATETIME DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY (round_id, reviewer, event))` — no FK on `reviewer` (same reason as proposals). Copy `round_vote` rows as `accept`, revision 1; drop `round_vote`.
- `pub enum Verdict { Accept, Reject, Unsure }`; `set_verdicts(pool, id, reviewer, revision, &[(String, Option<Verdict>)])` (`None` clears); `verdicts(pool, id) -> Vec<VerdictRow { reviewer, event, verdict, revision }>`.
- Routes: `PUT …/rounds/{round}/verdicts` body `{verdicts: {event_uuid: "accept"|"reject"|"unsure"|null}}` (merge); `PUT …/rounds/{round}/verdicts/{event}` body `{verdict}`; `GET …/rounds/{round}/verdicts` → `{revision, events: [{event, accept, reject, unsure, by?: [{reviewer, verdict, revision}]}]}` — counts are current-revision only, `by` only when readable. The existing `PUT/GET …/votes` keep working through the compatibility rule in Global Constraints. Event ids must parse as uuids (422 otherwise).

- [ ] **Step 1: Failing tests:** `a_reviewer_sets_one_verdict_and_changes_it`, `bulk_verdicts_merge`, `the_old_vote_call_sets_accepts_and_keeps_rejects`, `verdicts_are_refused_outside_voting_and_to_non_voters`, `blind_verdicts_show_only_to_the_reviewer_deciders_and_owners`, `a_new_revision_restarts_progress`, `existing_votes_become_accepts`.
- [ ] **Steps 2–5** as above.

### Task 3: Candidates from a catalogue

**Files:** Create `migrations/0027_round_candidates.sql`. Modify `rounds.rs`, `rounds_api.rs`, `access.rs`, `tests/proposals.rs`, `tests/rounds_api.rs`.

**Interfaces — produces:**
- `round_proposal.candidates INTEGER NOT NULL DEFAULT 0`; at most one candidates row per round (partial unique index). Its author may also own an ordinary proposal (the primary key becomes `(round_id, author, candidates)`; rebuild the table in the migration).
- Create body gains `candidates: {project, catalogue}`: the server checks the creator's role on `project` (`role_in`, public included), copies the catalogue's live events into a new proposal document as one catalogue named like the source, keeping uuids, recording the copy hop with the existing copy-chain code, then — when the round has no proposers — moves it to `voting` logged by the creator.
- `rules::proposal_role` for a candidates proposal: never writable; readable by deciders and owners always, by every member once the state is not `proposing`.
- `RoundOut` gains `candidates: Option<{doc: Option<Uuid>, source: {project, catalogue}, count}>`.

- [ ] **Step 1: Failing tests:** `a_review_round_copies_its_candidates_and_starts_reviewing`, `candidates_keep_the_source_ids_and_name_their_source`, `candidates_are_a_snapshot` (edit the source after creation; candidates unchanged), `nobody_writes_the_candidates`, `a_reviewer_without_access_to_the_source_reads_the_candidates`, `a_source_the_creator_cannot_read_is_refused` (404 like other unreadable rooms), `trashed_source_events_are_not_candidates`.
- [ ] **Steps 2–5** as above.

### Task 4: Python

**Files:** `crates/cocat-client/src/rounds.rs`, `crates/cocat-py/src/rounds.rs`, `crates/cocat-py/python/cocat_rs/client.py`, `__init__.py`, `raw.py`, `crates/cocat-py/tests/test_rounds.py`.

**Interfaces:** `create_round(..., kind=None, candidates=None)` where `candidates` is a `Catalogue` object or `(project_id, catalogue_uuid)`; `move_round(..., note=None)`; `verdict(round_id, event, value)` (`value` in `"accept"|"reject"|"unsure"|None`); `verdicts(round_id)` returning the GET shape; Project methods mirror Client ones.

- [ ] **Step 1: Failing tests:** `test_a_pipeline_reviews_its_detections` (pipeline saves 5 events in its own project; creates a review round in the team project with `candidates=catalogue` and two reviewers; reviewers give verdicts; `verdicts()` matches the pipeline's own uuids with counts), `test_a_move_carries_a_note`.
- [ ] **Steps 2–5** as above, with the Python suite.

### Task 5: The rounds list explains itself

**Files:** Modify `crates/cocat-web/src/rounds.rs` (JSON shapes: kind, revision, notes, candidates), `views/rounds.rs`, `views/catalogue.rs` (final-catalogue marker), the Help page view (find it under `views/`), `style.css`.

**Interfaces:** pure helpers in `rounds.rs`: `fn your_part(round: &Round, me: &str, progress: Option<Progress>) -> Option<String>` (texts: "Propose your intervals before {deadline}", "Propose your intervals", "Review {total} detections ({done} done)", "Review the proposals ({done} of {total} reviewed)", "Decide: {k} of {n} people have proposed", "Decide: {done} of {total} reviewed"), `fn group(rounds, me) -> (needs_you, in_progress, finished)`, `fn state_words(kind, state) -> &str` ("reviewing" for review rounds in `voting`).
- Rounds page: the intro sentence (Global Constraints), three groups, each round with kind and state in words; round page opens with the "Your part" box, shows notes in the log.
- Final catalogue marker: a catalogue whose `round` attribute parses shows "Decided in round {name}" linking to `#/rounds/{id}`.
- Help: a "Selection rounds" section (what a round is, the three kinds, blind vs open, the budget, verdicts).

- [ ] **Step 1: Failing tests** (pure): `your_part_tells_each_role_what_to_do` (one case per text above), `rounds_are_grouped_by_what_they_need_from_me`, `a_review_round_says_reviewing`, `a_final_catalogue_names_its_round`.
- [ ] **Steps 2–5** as above (+ `trunk build --release`). The controller checks the pages in Firefox.

### Task 6: Starting a round from the page

**Files:** Create `crates/cocat-web/src/views/round_form.rs`. Modify `views/rounds.rs`, `views/mod.rs`, `crates/cocat-web/src/rounds.rs`, `route.rs` (`#/rounds/new`), `style.css`.

**Interfaces:** `pub struct Preset { kind, visibility, budget_form: bool, candidates: Need, default_proposers: Defaults }` with `pub const PRESETS: [Preset; 3]` per the spec's preset table (data, not branches); `fn budget_json(form: &BudgetForm) -> Value` building the spec's JSON-LD (units chosen from `["byte","kbyte","Mbyte","Gbyte","Kibyte","Mibyte","Gibyte","bit","kbit","Mbit"]` and per-time `["s","min","h"]`), validated with `cocat_core::round::parse_budget` before sending; the candidates picker lists catalogues the visitor can read (this project's, linked projects', public ones) or "Import a file…" which runs the existing import (views/transfer.rs) into this project and selects the new catalogue. Only owners and editors see "New round".

- [ ] **Step 1: Failing tests** (pure): `each_preset_fills_the_form` , `the_budget_form_makes_the_spec_json_ld`, `a_budget_with_a_rate_in_the_wrong_unit_is_refused_before_sending`, `the_new_round_route_round_trips`.
- [ ] **Steps 2–5** as above. Controller checks in Firefox.

### Task 7: Proposing in the catalogue view

**Files:** Modify `views/rounds.rs`, `views/links.rs` or a small shared helper it already uses to show another room's catalogue live, `route.rs` (`#/rounds/{id}/proposal`).

**Interfaces:** "Your part" for a proposer offers "Open my proposal": `POST …/proposal` then shows the proposal room's single catalogue in the normal catalogue view (table, timeline, event editor, import), editable while `proposing`, read-only after (the server's role header decides). A first-time proposal gets a catalogue named "{round name}: {my email}" created in it.

- [ ] **Step 1: Failing tests** (pure where possible): `the_proposal_route_round_trips`, `a_new_proposal_gets_its_catalogue_name`; one end-to-end Firefox flow by the controller.
- [ ] **Steps 2–5** as above.

### Task 8: Reviewing and deciding

**Files:** Create `crates/cocat-web/src/views/review.rs`, `crates/cocat-web/src/views/decide.rs`, `crates/cocat-web/src/review.rs` (pure). Modify `route.rs` (`#/rounds/{id}/review`, `#/rounds/{id}/decide`), `views/rounds.rs`, `views/mod.rs`, `style.css`.

**Interfaces (pure, in `review.rs`):** `fn rows(candidates_and_proposals, verdicts, me) -> Vec<ReviewRow>`; `fn next_unreviewed(rows, from) -> Option<usize>`; `fn progress(rows) -> (done, total, set_aside)`; `fn majority(rows) -> Vec<bool>` (accepts > rejects, ties excluded); `fn final_events(rows, picked, emails) -> Vec<EventRecord>` (fresh uuids, attributes per Global Constraints); `fn final_catalogue_uuid(round_id) -> String` (uuid v5).
- Review page: table over the candidates proposal and every readable proposal, columns per spec, keys per Global Constraints, filter "not reviewed / all", counter text; each key press `PUT …/verdicts/{event}`; selected row kept in the URL (`?row=`).
- Decide page (deciders, in `voting`): rows sorted by accepts, "Keep what a majority accepted", budget gauge via `cocat_core::round::cost` on the ticked rows, "Decide" writes the catalogue with `final_catalogue_uuid` and `final_events` into the project document (replacing its events if it exists), then `POST …/state` decided.

- [ ] **Step 1: Failing tests** (pure): `the_next_unreviewed_row_follows_a_verdict`, `progress_counts_this_revision_only_and_sets_aside_removed_events`, `majority_keeps_more_accepts_than_rejects`, `final_events_carry_their_candidate_and_counts`, `deciding_again_rewrites_the_same_catalogue` (same uuid from the same round; `final_events` replaces), `the_review_and_decide_routes_round_trip`.
- [ ] **Steps 2–5** as above. Controller runs one end-to-end Firefox flow: pipeline catalogue → review round → two reviewers by keyboard → decide → marker on the final catalogue.
