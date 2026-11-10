# Projects hold catalogues and files (the merge) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A project is one thing: it always has its catalogues, and it holds a SciQLop
workspace's files once it has a file list (manifest). No project "kind" decides what it may
hold any more.

**Architecture:** What used to be `kind = workspace` becomes "has a manifest document". The
server drops the kind gates (bare socket, documents, blobs, public copy) and lets any
project create its manifest once. The API and the app page say `files: true|false`. The web
app has one project page: the catalogue browser, with a "Files" section in the sidebar and
the file views in the main pane when the project has files.

**Tech Stack:** Rust (axum, sqlx, Leptos CSR), yrs.

**Spec:** `docs/superpowers/specs/2026-11-10-workspace-sharing-design.md`, section "Projects
hold catalogues and files (merged 2026-11-10)". Parts 1–2 plans:
`2026-11-10-workspaces-server.md`, `2026-11-10-workspaces-web-viewer.md`.

## Global Constraints

- No change outside cocat-rs (the user: no modification to SciQLop or its plugins yet).
- Every project has its catalogues document at `/room/{project}`; today's clients keep
  working on every project.
- A project has at most one manifest. Personal projects may have one (they stay unshareable).
- The `room.kind` column stays in the database, unread (no migration needed; workspaces exist
  only on cocat-dev). `// simplify:` notes it; drop it in a later migration.
- UI words: the files side is "Files"; the menu item that makes a project with files stays
  "New SciQLop workspace" (it makes a project and its file list); help explains that a
  project can hold catalogues, a SciQLop workspace's files, or both.
- Project-wide timeline across documents stays later (History in the header is the
  catalogues' history; each file keeps its own History).
- Process rules (scratchpad `p-rules.md`); gate incl. wasm clippy (+ pytest if cocat-py
  changes); views checked in the browser by the controller; commit on main; never push.

## Review Focus

1. A project made with "New SciQLop workspace" also has working catalogues (bare socket 200, add a catalogue, it syncs). → Task 1 test, Task 2 browser.
2. A plain project gains a file list once (`POST documents {kind: manifest}` 201, the second 409) and then shows Files. → Task 1 test, Task 2 browser.
3. Copying a public catalogue into a project with files works (the old 409 is gone). → Task 1 test.
4. A viewer of a project with files sees both sections, no write controls. → Task 2 browser.
5. Old projects (no manifest) look exactly as before: no Files section, no extra requests that fail. → Task 2 browser.

---

### Task 1: The server has one kind of project

**Files:** `crates/cocat-server/src/{lifecycle.rs,documents.rs,blobs.rs,ws.rs,submissions_api.rs,api.rs,db.rs,ui/app.rs}`, `templates/app_head.html`; tests `tests/workspaces.rs`, `tests/blobs.rs`, `tests/submissions.rs`, `tests/ui_app.rs`, `tests/access_matrix.rs` as needed.

**Interfaces (produces):**
- `db::has_files(pool, room_id) -> Result<bool, DbError>` = a manifest document exists.
- `POST /api/rooms/{id}/documents` accepts `kind: "manifest"` when the project has none (201), 409 "This project already has its file list" otherwise; notebook/text documents and blobs need the manifest first (409 "This project has no files yet") — replaces `require_workspace`.
- Bare socket `/room/{project}` admits every project (the workspace 404 goes).
- Public copy into any project (the `require_catalogues` gate goes).
- `GET /api/rooms` rows and the create answer carry `files: bool` (the `kind` field may stay for one release, documented as deprecated — implementer's call, say which).
- App page meta: `<meta name="cocat-files" content="true">` when the project has files (replaces `cocat-kind`).
- `POST /api/rooms {name, kind: "workspace"}` keeps working: it makes the project and its manifest (the "New SciQLop workspace" path).

- [ ] Failing tests: `a_project_with_files_still_has_its_catalogues` (bare socket 101 on a project made with files), `a_plain_project_gains_its_file_list_once`, `files_need_the_file_list_first` (notebook doc / blob on a project without manifest → 409), `a_public_catalogue_copies_into_a_project_with_files`, `the_project_list_says_which_projects_have_files`, `the_app_page_says_when_a_project_has_files`.
- [ ] Remove the kind gates; implement; existing workspace tests updated to the new rules (not weakened: each old refusal either still holds under the new rule or its test is replaced by the new behaviour's test — list them in the report); gate; commit.

### Task 2: One project page with catalogues and files (controller)

**Files:** `crates/cocat-web/src/{session.rs,projects.rs,app.rs}` (`files: bool` replaces `Kind`), `views/workspace.rs` → split into `views/files.rs` (the Files sidebar section, the file pane, history) used by `App`; `views/sidebar.rs` (Files section below the catalogues when `files`), `views/header.rs` (History always shown: catalogue history), `style.css`.

- [ ] Native tests: `RoomConfig.files` from the meta (absent → false), `Project.files` from the list.
- [ ] `App` connects the catalogues room as today and, when `files`, the manifest room too (not the context Room); the sidebar shows "Files" (tree) under the catalogues; `#file=<path>` shows the file pane in the main area; everything from part 2 keeps working (live, history, restore, safety).
- [ ] Browser: a project made with "New SciQLop workspace" (seeded with files) shows catalogues + Files; a catalogue added there syncs; an old project shows no Files section and no failing requests; a viewer sees both, no write controls; ws6/ws7 flows adapted and green.
- [ ] Gate; commit.

### Task 3: Words and docs (controller)

**Files:** `crates/cocat-server/templates/help.html` (Projects: a project holds catalogues, a SciQLop workspace's files, or both; the SciQLop workspaces section becomes "Files of a SciQLop workspace"), `tests/ui_help.rs`, `views/project_dialogs.rs` (hint: "A project with a SciQLop workspace's files: its notebooks and scripts, beside its catalogues."), README (Workspaces section → projects with files; API `files`), the spec's "Left to the builder"/plans references if stale.

- [ ] Help test first; write; gate; commit.
