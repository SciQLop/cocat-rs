# Progressive Loading Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Big catalogues and big history versions show their first rows within about a second and fill in progressively.

**Architecture:** The history route gains optional paging, per-catalogue counts and a small in-memory cache of computed diffs. The web history reads it 200 changes at a time. The web catalogue page shows a read-only preview fed by the existing paged events route while the project downloads, then swaps to the live table keeping scroll, selection and sort.

**Tech Stack:** Rust (axum, yrs, utoipa, leptos), an established LRU crate if one is already a dependency, else a small `simplify:`-noted map with expiry.

**Spec:** `docs/superpowers/specs/2026-11-11-progressive-loading-design.md`

## Global Constraints

- History route `GET /api/rooms/{project}/history/{version}`: optional `limit` (1..=10000) and `cursor`; with `limit` the answer is `{counts, catalogues: [all catalogue changes], events: [first limit event changes], next}`; without it, today's answer plus `counts`. Python must keep working unchanged.
- `counts` = `{total: {added, changed, removed}, catalogues: [{catalogue, name, added, changed, removed}]}`; an event in several catalogues counts in each catalogue row and once in total.
- Change order: (start after the change, else before; then uuid); cursor encodes that pair.
- Diff cache: keyed by (project, start record, end record, catalogue); expires after 5 minutes; at most 16 entries.
- Web history page size: 200; next page requested when the list's end is near.
- Catalogue preview: first page 1000, next pages 10000, from `GET /api/rooms/{project}/catalogues/{catalogue}/events`; read-only; header text "Showing {n} of {total} events; the rest is loading…"; sorting off with headers titled "sorting once loaded"; at the switch the live table keeps scroll offset, selected event uuid and applies its sort. No swap when the project arrives before the first page. "All events" keeps today's loading note.
- One build/test at a time, foreground; full gate before each commit (fmt, clippy native, `cargo test --workspace`, clippy wasm; web tasks add `trunk build --release`; Task 1 adds the Python suite). Commit to `main` with explicit paths; never push.

## Review Focus

1. Python history calls (no `limit`) still get every change (Task 1 test `without_a_limit_every_change_comes_as_before`).
2. Pages of one version never repeat or skip a change when two events share a start (Task 1 test `pages_neither_repeat_nor_skip_events_with_the_same_start`).
3. A version still growing (the open one) is not served stale from the cache (Task 1 test `the_open_version_is_not_served_from_the_cache_after_new_edits`).
4. Opening a small project shows no preview flash (Task 3 pure test `no_preview_when_the_project_arrives_first`).
5. The selected row survives the swap (Task 3 pure test `the_selection_survives_the_switch`).

---

### Task 1: Paged history with counts and a diff cache

**Files:** Modify `crates/cocat-server/src/history_api.rs`, `crates/cocat-server/src/history/diff.rs`; create `crates/cocat-server/src/history/cache.rs`; tests in `crates/cocat-server/tests/` (existing history test file) and Python `crates/cocat-py/tests` history test untouched but run.

**Interfaces:** `pub struct Counts { total: Tally, catalogues: Vec<CatalogueTally> }`, `pub fn counts(diff: &Diff, before: &Doc, after: &Doc) -> Counts` (or computed inside `diff`), `pub fn page(diff: &Diff, after: Option<(i128, String)>, limit: usize) -> (Vec<EventChange>, Option<String>)`; cache `DiffCache::get_or_compute(key, f)`, held in `AppState` or the history module (builder's call, one place).

- [ ] **Step 1: Failing tests:** `a_limited_answer_has_counts_catalogue_changes_and_a_first_page`, `the_next_page_continues_where_the_first_stopped`, `pages_neither_repeat_nor_skip_events_with_the_same_start`, `an_event_in_two_catalogues_counts_in_each_and_once_in_total`, `without_a_limit_every_change_comes_as_before`, `a_second_page_does_not_rebuild_the_project` (cache hit observable via a counter or timing-free hook), `the_open_version_is_not_served_from_the_cache_after_new_edits`, `a_bad_limit_or_cursor_is_refused` (422).
- [ ] **Steps 2–5:** RED, implement, gate (+ Python suite), commit.

### Task 2: History view loads progressively

**Files:** Modify `crates/cocat-web/src/views/history.rs`, `crates/cocat-web/src/history.rs` (JSON shapes), `crates/cocat-web/src/api.rs` if needed.

**Interfaces:** the changes table asks `limit=200`, shows the counts summary first ("{changed} changed, {added} added, {removed} removed" per catalogue and total), appends the next page when the visitor scrolls near the end; "Loading more changes…" line while a page is in flight.

- [ ] **Step 1: Failing tests** (pure): `the_summary_reads_the_counts`, `the_next_page_is_asked_near_the_end`, `pages_are_appended_in_order`.
- [ ] **Steps 2–5** as above (+ trunk build). Controller checks a STIX-sized version in Firefox.

### Task 3: Catalogue preview while the project downloads

**Files:** Modify `crates/cocat-web/src/views/catalogue.rs`, `views/table.rs`, `connection.rs` (expose loading state if needed); create `crates/cocat-web/src/preview.rs` (pure) and `views/preview.rs` if the view grows.

**Interfaces:** pure `fn preview_due(loading: &Loading, first_page_arrived: bool) -> bool`, `fn switch_state(preview: &PreviewState) -> TableState { scroll_top, selected: Option<String> }`; the table renders from either the preview rows or the live replica through one component.

- [ ] **Step 1: Failing tests** (pure): `no_preview_when_the_project_arrives_first`, `the_header_says_how_much_is_shown`, `pages_append_in_start_order`, `the_selection_survives_the_switch`, `sorting_is_off_in_the_preview`.
- [ ] **Steps 2–5** as above (+ trunk build). Controller checks STIX in Firefox (first rows within about a second, swap without a jump).
