# Links to another project's catalogues — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A project can show a catalogue of another project as a link. Opening it opens the
original, live, with the visitor's role in the original; edits go there. Nothing is copied.

**Architecture:** Links are data in the linking project's catalogues document: a root map
`links`, link id → `{project, catalogue, name, by, at}`. The core reads and writes it. The
web app lists links in the sidebar and, on `#link=<id>`, mounts the catalogue page inside a
nested context holding a second room for the original project (read/write by the visitor's
role there). The server is unchanged: listing another project's catalogues already exists
(`GET /api/rooms/{id}/catalogues`, members only), and opening the original checks access.

**Tech Stack:** Rust (cocat-core, cocat-web/Leptos), yrs.

**Spec:** docs/superpowers/specs/2026-11-10-workspace-sharing-design.md ("Links (decided
2026-11-10)") and the conversation: links, not two-way synced copies; your role in the
original applies; lost access shows "No longer available"; a personal project may hold links
(it still shares nothing); links to files come later; smart catalogues cannot use a linked
catalogue in their rules at first.

## Global Constraints

- No change outside cocat-rs (the user: no modification to SciQLop or its plugins yet).
- Root map name `links`; an entry: `{project: <id>, catalogue: <uuid>, name: <catalogue name when linked>, by?: <email>, at: <integer ms since epoch>}`; link id = a new uuid.
- A link to a catalogue of the same project is refused (it is already there); a project links a given (project, catalogue) at most once.
- Older clients ignore `links` (it is a new root map); exports never include links.
- Viewers of the linking project see its links; whether they can open one depends on their own access to the original.
- UI words: "Link a catalogue from another project…", "from <project>", "Remove link", "No longer available: you are not in <project> any more, or the catalogue was deleted."
- Process rules (scratchpad `p-rules.md`); gate incl. wasm clippy; views checked in the browser by the controller; commit on main; never push.

## Review Focus

1. Opening a link whose original you lost access to (or that was deleted): "No longer available" with Remove link, no reconnect loop. → Task 2 browser + a native test of the availability rule.
2. A viewer in the original project opens a link from their own project: read-only (no New event, no edits). → Task 2 browser.
3. An edit made through the link appears in the original project for its other members, live. → Task 2 browser (two clients).
4. Linking the same catalogue twice, or one of the same project: refused with a plain message. → Task 1 tests.
5. A link survives renaming the original catalogue (the sidebar shows the current name once the original is reachable, else the saved one). → Task 2.

---

### Task 1: Links in the core

**Files:** create `crates/cocat-core/src/model/links.rs`; modify `model/mod.rs` (export); tests `crates/cocat-core/tests/links.rs`.

**Interfaces (produces):**
- `pub struct Link { id: String, project: String, catalogue: String, name: String, by: Option<String>, at: i64 }` (serde, Clone, PartialEq, Debug).
- `pub fn links<T: ReadTxn>(txn: &T) -> Vec<Link>` (sorted by name, case-insensitive; malformed entries skipped).
- `pub fn add_link(txn: &mut TransactionMut, this_project: &str, link: NewLink) -> Result<String, LinkError>` — refuses `SameProject` and `AlreadyLinked`; returns the new id.
- `pub fn remove_link(txn: &mut TransactionMut, id: &str) -> bool`.

- [ ] Failing tests: `a_project_without_links_has_none`, `a_link_is_kept_with_who_and_when`, `the_same_catalogue_is_linked_once`, `a_project_does_not_link_its_own_catalogue`, `removing_a_link_leaves_the_others`, `a_malformed_link_is_skipped`, `links_never_reach_an_export` (tscat JSON / VOTable of the project's catalogues unchanged by a link).
- [ ] Implement; gate; commit.

### Task 2: Links in the web app (controller)

**Files:** `crates/cocat-web/src/route.rs` (`Route::Link(String)` ⇄ `#link=<id>`, native test), `src/edit.rs` (add/remove wrappers with the user's email and now ms, native tests), `views/sidebar.rs` (a "Linked" group under the catalogues: link mark, name, "from <project>"; ⋯ → Remove link for editors; "+" menu → "Link a catalogue from another project…"), create `views/links.rs` (the picker dialog: projects the visitor is in, except this one, then that project's catalogues via `GET /api/rooms/{id}/catalogues`; and `LinkedCatalogue`: resolves the original project in the visitor's project list, builds its `RoomConfig` with `for_project`, connects a second room, provides it as the context Room for its subtree only, renders the catalogue page for `Route::Catalogue(<uuid>)` with a line "Linked from <project>: changes go there."; unavailable → the No-longer-available text + Remove link), `app.rs` (main pane on `Route::Link`), `style.css`.

- [ ] Native tests (route round trip; edit wrappers; the availability rule as a pure function: `link_state(link, projects) -> Reachable(Project) | NotAMember`).
- [ ] Implement; browser: link a catalogue from Lab into My catalogues, open it, add an event as editor and see it in Lab from a second client, a viewer can't edit, remove access → No longer available, Remove link; light + dark desktop screenshots.
- [ ] Gate; commit.

### Task 3: Help and README (controller)

- [ ] Help: in Catalogues, a paragraph on links (what a link is, that changes go to the original, your role there applies, smart catalogues can't use a link yet). README: the `links` root map. Help test first; gate; commit.
