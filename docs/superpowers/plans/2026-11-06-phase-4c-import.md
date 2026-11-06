# Phase 4c — Import from a Python cocat server — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `cocat-server import` moves a running Python cocat deployment to the Rust server
in one idempotent command, without touching the Python data.

**Architecture:** One `import` module and one CLI subcommand. It reads the Python SQLite
database read-only (`?mode=ro`) with sqlx, reads the `.y` room files with the existing
`read_updates` (never truncating), and writes the Rust database in one transaction. Room
files stay where they are: the Rust server reads the same `.y` format from the same
directory. Every step is an upsert keyed on the Python ids, so running it twice changes
nothing the second time.

**Tech Stack:** new: `bcrypt` (established) to verify the bcrypt hashes older Python
accounts may hold; existing `argon2` for PHC hashes.

**Spec:** design section "Import (one-shot, before the cutover)":
`cocat-server import --update-dir <dir> --db <path> [--users <python.db>] [--owner <email>]
[--dry-run]`. Behaviour reference for the Python data: the Python test checkout
`src/cocat/app/db.py` and `src/cocat/app/migrations/versions/0001..0004` (read them all:
they are the shapes the import must accept).

## Global Constraints

- This repo, branch `main`, commit per task, never push.
  Rules file, TDD, one build/test command at a time in the foreground, full gate after the
  last edit of each task.
- **Two Python schemas exist and both must import:**
  1. *Released* (cocat ≤ 0.15 on PyPI, no `alembic_version` or version `0001`): users
     only, each with a `rooms` JSON list. Port `0002_rooms.py::_migrate_user_rooms`
     exactly: every listed room becomes a room named by its id; every listing user an
     editor; then the owner is picked by `_pick_owner` (port it, same rule); a `rooms`
     value that isn't a list of strings is skipped with a warning naming the email. Port
     `0003` too: accounts from the released schema are imported as verified.
  2. *Rooms model* (alembic `0002`..`0004`): copy `room`, `membership`, `invitation`
     (`token_hash` is SHA-256 hex in both; copy as-is so live links keep working) and
     `oauth_account` (`oauth_name` → `provider`, `account_id` → `subject`; provider tokens
     are dropped).
  Detect which by the presence of the `membership` table, not by version strings alone.
- Users: copy id (Python stores UUIDs as 36-char strings; the Rust schema uses the same
  text form: check and convert if not), email, flags, `hashed_password` as-is. Login then
  accepts both: `auth::verify_password` learns bcrypt (`$2a$`/`$2b$`/`$2y$`) through the
  `bcrypt` crate, and a successful bcrypt login rehashes to Argon2 in the same request.
- Every room file in `--update-dir` whose name is a valid room id becomes a room if the
  database doesn't already list it (name = id). Without `--users`, `--owner <email>` (an
  existing Rust user) owns every imported room; with neither, refuse with a clear error.
- A room file that doesn't read (bad header) is reported and skipped, never modified.
- `--dry-run` prints the same summary (users, rooms, memberships, invitations, linked
  accounts, skipped items with reasons) and writes nothing.
- The Python database is opened read-only; the import refuses to run if `--db` and
  `--users` name the same file.

## Review Focus

1. **Running it twice** creates no duplicates and changes no roles.
2. **A Rust database already in use** (a user registered on Rust with the same email as a
   Python user): the import stops before writing anything and names the conflicts; it
   doesn't merge accounts silently.
3. **A released-schema user whose `rooms` is not a list** (null, a string): skipped with a
   warning; the rest imports.
4. **A bcrypt account logs in on Rust** and its hash becomes Argon2.
5. **Live invitation links from Python keep working on Rust** (same token, same hash).

---

### Task 1: bcrypt passwords

**Files:** `crates/cocat-server/src/auth.rs`. Test: `tests/auth.rs`.
- [ ] Tests: `bcrypt_hash_verifies` (a hash made by Python's `bcrypt`, committed as a
  constant), `bcrypt_login_rehashes_to_argon2`, `wrong_password_on_bcrypt_refused`,
  `unknown_hash_format_refused`. Remove the `simplify:` comment that named this gap.
- [ ] Red, implement, gate. Commit "Accept bcrypt hashes and rehash them on login".

### Task 2: The import

**Files:** create `crates/cocat-server/src/import.rs`; modify `cli.rs`. Fixtures:
`crates/cocat-server/tests/fixtures/python/{released.db,rooms_model.db}` built once by a
committed script `make_python_dbs.py` run with the Python test checkout's venv (released
schema: build it by hand with the 0001 table shapes; rooms model: the Python CLI and app),
plus two small `.y` files. Test: `tests/import.rs`.
- [ ] Tests: `imports_released_schema` (rooms from JSON lists, owner picked as Python
  does, everyone verified), `imports_rooms_model` (memberships, roles, invitations,
  oauth accounts), `python_invitation_link_accepts_on_rust` (review focus 5),
  `rooms_from_files_only_with_owner`, `no_users_no_owner_refused`, `second_run_changes_nothing`
  (review focus 1), `email_conflict_stops_before_writing` (review focus 2),
  `bad_rooms_value_skipped` (review focus 3), `bad_room_file_skipped_untouched`,
  `dry_run_writes_nothing`, `same_file_refused`, `python_db_opened_read_only` (the file's
  mtime and bytes are unchanged).
- [ ] Red, implement, gate. Commit "Import a Python cocat deployment".

### Task 3: End to end, and docs

- [ ] A script `bench/correctness/import.py`: start the *Python* server on a fresh
  directory, create users, rooms, an invitation and edits through the Python CLI and API,
  stop it; run `cocat-server import`; start the Rust server on the same update dir and the
  new database; check a Python account logs in, sees its rooms with the same roles, the
  invitation link accepts, and each room's document equals what the Python client wrote
  (state JSON). Run it once, paste the output in the report.
- [ ] README "Moving from the Python server": the command, the two schemas, what's kept
  and dropped (provider tokens), the read-only guarantee, and the advice to keep the Python
  database until the Rust server has run for a while. Commit "Document the import".
