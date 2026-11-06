# Phase 4a — Access matrix and frame fuzzer — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The deterministic half of the security campaign: one test that tries every route
with every kind of caller and checks the answer against a written table, and a fuzzer that
throws malformed and hostile websocket frames at a room.

**Architecture:** The matrix is *data*: a table of `(method, path template, minimum
access)` rows, one per route the server exposes, plus a list of callers (anonymous,
unverified, deactivated, stranger, viewer, editor, owner, group viewer, group editor, group
owner, superuser). One test expands the table, seeds a fresh server per caller kind, and
asserts each answer falls in the expected class (allowed, 401, 403, 404, redirect to login).
A second test makes the table complete: it walks axum's router (or a list the router is
built from) and fails when a route is missing from the table, so a new route can't skip
the matrix. The fuzzer uses `proptest` to generate y-sync messages (valid shapes with
hostile contents, and raw garbage), sends them through real sockets, and checks the
invariants below.

**Tech Stack:** test-only: `proptest` (established; generators and shrinking). No new
runtime dependency.

**Spec:** design phase 4: "a websocket frame fuzzer, and a role × route access matrix test.
Every claimed finding is verified with a reproducing test before it is fixed; the
reproducers stay in the suite."

## Global Constraints

- This repo, branch `main`, commit per task, never push.
  Rules file, TDD where code changes, one build/test command at a time in the foreground,
  full gate after the last edit of each task.
- **A failure found by these tests is a finding, not a test to loosen.** Fix it in the code
  with its own reproducer test (named after the bug) in the module's own test file, in a
  separate commit, and list it in the report. Only change a matrix row when the row was
  wrong; say why in the report.
- Fuzz runs in the normal suite must stay under 30 s in total (`PROPTEST_CASES` default kept
  small, e.g. 64); a `FUZZ_CASES` env var raises it for a long run.

## Review Focus

1. **A route that answers 200 to a stranger** for a room or group they're not in.
2. **A viewer whose write changes the document** through any frame shape (SyncStep2,
   Update, a SyncStep1 carrying an update, a frame with trailing bytes after a valid
   message).
3. **A frame that panics a room thread, or wedges it** so other clients stop receiving.
4. **A frame that makes the server allocate without bound** (a huge length prefix, a deep
   nesting).
5. **A UI POST that acts for a caller the matching API route refuses.**

---

### Task 1: The access matrix

**Files:** create `crates/cocat-server/tests/access_matrix.rs` and
`crates/cocat-server/tests/access_matrix/table.rs` (or one file if short).

- Callers: anonymous; unverified (when mail is configured); deactivated with a still-valid
  cookie; stranger (verified, no relation); room viewer / editor / owner (direct); room
  viewer / editor through a group; group member; group owner; superuser.
- Expected classes: `Allowed` (2xx or the route's documented redirect), `Unauthorized`
  (401 for `/api` and `/auth`, 307 to `/ui/login?next=` for UI pages), `Forbidden` (403),
  `Hidden` (404).
- One row per route, covering `/api/*`, `/auth/*`, `/users/*`, `/rooms`, `/room/{id}/users`,
  the websocket upgrade `/room/{id}` (handshake status), and every `/ui/*` GET and POST.
  The row says the minimum caller that is `Allowed` and what everyone below gets.
- Each request uses ids that exist (a seeded room, group, member, invitation) so a 404
  means "hidden", never "no such thing".
- `every_route_is_in_the_matrix`: compares the table with the router's routes. If axum
  can't list them, build the router from a `const ROUTES` list in the code that both the
  router and the test read; prefer the smallest change that makes completeness checkable.
- [ ] Write the table and the test. Run it. Every mismatch is either a wrong row (fix the
  row, explain) or a finding (separate fix commit with a reproducer).
- [ ] Gate. Commit "Test every route against every kind of caller".

### Task 2: The websocket frame fuzzer

**Files:** create `crates/cocat-server/tests/frame_fuzz.rs`; `proptest` dev-dependency.

- Generators: (a) structured: y-sync `Message`s (SyncStep1 with random state vectors,
  SyncStep2/Update with random update bytes, valid updates built from random edits on a
  scratch `Doc`, awareness, auth, unknown message types), optionally with trailing bytes;
  (b) raw: arbitrary bytes up to 64 KiB, length prefixes claiming up to `u64::MAX`.
- Invariants, checked after each sequence:
  1. A viewer's frames never change the room state (compare an editor's view before and
     after).
  2. After any sequence, a fresh client still syncs and a valid editor update still
     reaches another client within 2 s (the room thread is alive and not wedged).
  3. The server process's RSS stays under a bound (e.g. +256 MB over the run) — read
     `/proc/self/statm` in-process since the server runs in the test process.
  4. The room file reopens with `FileRoomStore::open` to the same state the room serves.
- [ ] Write the fuzzer, run with the default case count, then once with
  `FUZZ_CASES=2000` and report the time and result. Findings as in Global Constraints.
- [ ] Gate. Commit "Fuzz the websocket with hostile frames".

### Task 3: Report

- [ ] In `docs/superpowers/reports/2026-11-06-phase-4a.md`: the matrix size (rows ×
  callers), the fuzz case counts and times, every finding (what, reproducer test name, fix
  commit), and every matrix row that was corrected and why. Commit "Report the access
  matrix and fuzz results".
