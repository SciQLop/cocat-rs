# The copy chain: where a catalogue was copied from — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every catalogue made as a copy of another records the whole chain of copies it
descends from (oldest first), so a later "propose changes back" can find every ancestor.

**Architecture:** A new key `copied_from` in the catalogue's CRDT map: a Y Array of maps,
one per hop. Copying appends the source as the last hop to the source's own chain. The core
owns the shape (read, extend, cap); each copy path (server, web, Python via the core) passes
the hop it knows. The field rides along in every existing state copy (`restore` copies map
keys), except where `adopt` must keep the target's own chain.

**Tech Stack:** Rust (cocat-core, cocat-server, cocat-web, cocat-py), yrs.

**Spec:** The user's request (2026-11-10): "a chain of all the successive copies (a bit like
origin IP when a request goes through several web proxies), so a PR-like feature can be
built later." Context: docs/superpowers/specs/2026-11-10-workspace-sharing-design.md (links
and forks are discussed in the conversation; this plan is only the chain).

## Global Constraints

- Field name `copied_from` (not `origin`/`derived_from`: those mean what made an event).
- A hop: `{project: <id>, project_name, catalogue: <uuid>, catalogue_name, version?: <u64 history version at copy time>, at: <integer ms since epoch>, by?: <email>}`.
- Order: oldest first; the source of this copy is the last hop.
- Cap: 32 hops; when longer, keep the first hop (the original) and the latest 31.
  `// simplify:` names it.
- Existing catalogues without the key read as an empty chain; older clients ignore the key
  (it has no `#` prefix, so it is not an attribute and does not reach tscat/TFCat exports).
- `adopt` (accepting a submission into a public catalogue) keeps the public catalogue's own
  `copied_from`; it never takes the proposal's.
- Process rules (scratchpad `p-rules.md`); gate incl. wasm clippy and pytest when cocat-py
  changes; commit on main with explicit paths; never push.

## Review Focus

1. A copy of a copy of a copy: chain has 3 hops, oldest first, each naming the right project and catalogue. → Task 2 test.
2. Accepting an update into a public catalogue leaves its chain unchanged. → Task 1 test (adopt) + Task 2 test.
3. A freeze in the same project adds a hop naming the same project and the smart catalogue. → Task 3.
4. A malformed `copied_from` (not an array, hops missing fields) reads as what parses, never panics. → Task 1 test.
5. The 33rd hop keeps the original and drops the oldest after it. → Task 1 test.

---

### Task 1: The chain in the core

**Files:** Create `crates/cocat-core/src/model/copies.rs` (or a section in an existing model file if smaller); modify `model/mod.rs` (`CatalogueSummary.copied_from: Vec<Hop>`), `model/read.rs` (read it), `model/submission.rs` (`adopt` keeps the target's chain); tests `crates/cocat-core/tests/copies.rs`.

**Interfaces (produces):**
- `pub struct Hop { project: String, project_name: String, catalogue: String, catalogue_name: String, version: Option<u64>, at: i64, by: Option<String> }` (serde, utoipa `ToSchema` like the other model types).
- `pub fn copy_chain(txn, source_catalogue: &str, hop: Hop) -> Vec<Hop>` = source's chain + hop, capped.
- `pub fn set_copied_from(txn, catalogue: &str, chain: &[Hop]) -> Result<(), WriteError>` writes the Y Array.
- `CatalogueSummary.copied_from` filled by `summary`.

- [ ] Failing tests: `a_catalogue_without_a_chain_has_none`, `a_copy_extends_its_sources_chain`, `the_chain_keeps_the_original_and_the_latest`, `a_malformed_chain_reads_what_parses`, `adopting_a_proposal_keeps_the_targets_chain`.
- [ ] Implement; gate; commit.

### Task 2: The server's copy paths

**Files:** `crates/cocat-server/src/submissions_api.rs` (`copy_into`/`make_copy` paths of `copy` and `propose`: the copied catalogue gets hop {public id, public name, uuid, name, version = base, at, by = caller}; `new_public`: the new public catalogue gets hop {author's project id/name, uuid, name, version = snapshot base if known, at, by = author}), `crates/cocat-server/src/history_api.rs` (`copy` of a version: every catalogue in the new project gets hop {source project, its own uuid/name, version, at, by}); tests in `tests/submissions.rs`, `tests/history_api.rs`.

- [ ] Failing tests: `a_copied_public_catalogue_knows_where_it_came_from`, `a_copy_of_a_copy_lists_both_hops_oldest_first` (copy public → project A; submit A's as a new public catalogue; copy that into B: B's chain = [first public, A]), `accepting_an_update_keeps_the_public_chain`, `a_copied_version_names_its_source_project`.
- [ ] Implement (write the chain in the same update that writes the copy); gate; commit.

### Task 3: Freezing a smart catalogue

**Files:** `crates/cocat-core/src/model/materialize.rs` (takes the hop: `materialize(..., copied: Hop)` or the caller writes it — pick one, consistently), `crates/cocat-web/src/edit.rs` (`materialize` passes {this project, smart catalogue uuid/name, no version, at, by = user}), `crates/cocat-py/src/lib.rs` (`Catalogue.materialize` likewise); tests in core materialize tests and the web edit tests.

- [ ] Failing tests: `a_frozen_catalogue_names_the_smart_one_it_froze`; gate (incl. pytest); commit.

### Task 4: Show it

**Files:** `crates/cocat-py/src/lib.rs` + `python/cocat_rs/_native.pyi` (`Catalogue.copied_from` → list of dicts, read-only) and a pytest; `crates/cocat-web/src/views/rule.rs` (or the catalogue heading): a quiet line "Copied from <project> › <catalogue>, <date>" for the last hop, the full chain in its `title`; README (catalogue fields) and help (one sentence in Catalogues).

- [ ] Failing tests (pytest `test_a_copy_lists_where_it_came_from`; native web helper for the heading text if it is a pure function); implement; browser check (controller); gate; commit.
