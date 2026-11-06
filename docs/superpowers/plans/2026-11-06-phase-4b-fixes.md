# Phase 4b — Fixes from the attacker campaign — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix every finding the attacker agents proved (each reproduced twice on a fresh
instance by the reviewer), with a reproducer test per finding that fails before the fix.

**Architecture:** Server-side sessions replace the stateless session JWT; the cross-site
guard covers every state-changing request; password hashing leaves the async workers;
cold reads are bounded; the websocket message limit drops to Python's.

**Tech Stack:** nothing new. Session tokens reuse the invitation token helpers
(`invitations::new_token` / `invitations::hash`: 32 random bytes, URL-safe base64, SHA-256
hex stored). `tokio::sync::Semaphore` for cold reads, `tokio::task::spawn_blocking` for
hashing.

**Spec:** design phase 4: "Every claimed finding is verified with a reproducing test before
it is fixed; the reproducers stay in the suite." Findings: the attacker worktrees
`../cocat-rs-attack-N/attack/findings/` (read the one each task
names; its Python reproducer shows the exact requests).

## Global Constraints

- This repo, branch `main`, one commit per task, never
  push. Rules file, TDD (reproducer red first, named after the bug), one build/test command
  at a time in the foreground, full gate (`cargo fmt --all --check`, `cargo clippy
  --all-targets -- -D warnings`, `cargo test --workspace`) after the last edit of each task.
- The cookie *contract* doesn't change: name `fastapiusersauth`, path `/`, `Secure`,
  `HttpOnly`, `SameSite=Lax`, no `Max-Age` on login, `Max-Age=0` on logout. Only the value
  changes (opaque random token instead of a JWT). Clients never decode it.
- Reset, verification, OIDC-state tokens stay JWTs (they're one-shot links, already bound).
- Never touch the attacker worktrees.

## Review Focus

1. **A session that survives** logout, a password change (API, UI, reset link), a
   deactivation, an account deletion, or its expiry.
2. **A session token readable from the database**: only its SHA-256 is stored.
3. **A non-browser client refused** by the widened cross-site guard (no `Origin`, no
   `Sec-Fetch-Site` must still pass: the Python client and SciQLop send neither).
4. **A login whose timing still depends on whether the email exists.**
5. **A legitimate large update refused**: 16 MiB must still pass.

---

### Task 1: Server-side sessions (finding 1-01)

**Files:** create `migrations/0006_sessions.sql`; modify `auth.rs`, `accounts.rs`,
`ui/login.rs`, `ui/signup.rs`, `ui/oauth.rs`, `db.rs`. Test: `tests/sessions.rs`.

**Interfaces:**
- Table `session(token_hash TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES "user"(id)
  ON DELETE CASCADE, expires_at DATETIME NOT NULL)` + index on `user_id`.
- `auth::start_session(pool, user_id) -> Result<String, AuthError>`: new token, insert the
  hash with `expires_at = now + 3600 s` (same lifetime as today), delete this user's
  expired rows in the same statement batch. Replaces every `mint_token` call (API login,
  UI login, UI signup, OIDC callback).
- `auth::authenticate(pool, headers) -> Result<Uuid, AuthError>`: one query joining
  `session` and `"user"` on the hash, `expires_at > now`, user active. Drop the `secret`
  parameter and fix the callers.
- `auth::end_session(pool, headers)`: delete the row for the cookie. Called by
  `/auth/jwt/logout` and `/ui/logout` (the UI logout must now read the cookie).
- `db::end_sessions_of(executor, user_id)`: delete every row of a user. Called inside the
  same transaction as: a password change (`accounts::change` when the password changes),
  `accounts::reset_password`, a deactivation. Deletion is covered by the cascade.
  After a password change through `PATCH /users/me` or the UI account page, the caller's
  own session ends too (simplest correct rule; the UI then redirects to login with a
  message "Password changed, please sign in again").
- Remove `mint_token`, `verify_token`, `Claims`, `AUDIENCE`, `LIFETIME_SECONDS` if nothing
  else uses them; update the module doc (no longer "a cookie one server mints the other
  accepts": say Python cookies stop working at cutover and users sign in once).
- [ ] Tests: `logout_ends_the_session` (API and UI), `password_change_ends_every_session`,
  `password_reset_ends_every_session`, `deactivation_ends_every_session`,
  `deleting_the_user_drops_sessions`, `expired_session_refused`,
  `unknown_token_refused`, `only_the_hash_is_stored` (read the table, the cookie value is
  absent), `two_logins_are_two_sessions` (logout of one keeps the other).
- [ ] Red, implement, gate. README: the cookie paragraph and "Moving from the Python
  server" (users sign in again once). Commit "Keep sessions on the server".

### Task 2: Cross-site guard on every state change (finding 1-04)

**Files:** `ui/frame.rs` (`same_site`). Test: `tests/ui_csrf.rs` (or the existing UI
cross-site test file).
- The guard applies to every method other than GET/HEAD/OPTIONS on every path, not only
  POST under `/ui`. `cross_site` is unchanged (a request with neither header passes).
- [ ] Tests: `cross_site_login_form_refused` (the finding's request: form POST to
  `/auth/jwt/login` with `Origin: http://evil.test` → 403, no `Set-Cookie`),
  `cross_site_api_write_refused` (a PATCH and a DELETE under `/api` with
  `Sec-Fetch-Site: cross-site`), `client_without_browser_headers_still_logs_in`.
- [ ] Red, implement, gate. README: the CSRF sentence names the wider scope. Commit
  "Refuse cross-site writes on every route".

### Task 3: Password hashing off the workers, rate limit, equal-cost miss (findings 1-03, 06)

**Files:** `auth.rs`, `db.rs` (`hash_password`), `accounts.rs`, `accounts_api.rs`.
Test: `tests/auth.rs`, `tests/accounts_api.rs`.
- Every Argon2/bcrypt hash or verify runs in `spawn_blocking` (make `hash_password` and
  `verify_password` async wrappers, or add async variants and use them at every call site:
  grep `hash_password` and `verify_password`).
- `PATCH /users/me` joins the rate-limited routes (10/min per client, keyed by path like
  the others) — 429 above it.
- Finding 06 (found while reviewing): an unknown email verifies against `DUMMY_HASH`, which
  uses m=65536,t=3, while real hashes use `Argon2::default()` (m=19456,t=2): a miss takes
  ~110 ms and a wrong password ~30 ms, so login timing tells which emails have accounts.
  Replace the constant with a dummy hash made once at first use with `Argon2::default()`
  (a `std::sync::LazyLock`), so both paths cost the same.
- [ ] Tests: `profile_changes_are_rate_limited`, `hashing_does_not_block_other_requests`
  (start N=32 password changes on a current-thread-flavoured test server or measure that a
  concurrent `GET /api/me` answers while they run — pick the variant that is reliable, not
  timing-flaky; if no reliable variant exists, assert via a unit test that the hashing call
  sites go through the blocking wrapper and say so in the report), and
  `dummy_hash_uses_the_default_cost` (parse its PHC string, compare params with a fresh
  `Argon2::default()` hash).
- [ ] Red, implement, gate. Commit "Hash passwords off the async workers".

### Task 4: Bounded cold reads and websocket messages (finding 1-02, lead 1)

**Files:** `room.rs`, `ws.rs`. Test: in `room.rs` tests and `tests/catalogues_api.rs`.
- Cold reads (`RoomManager::read` when the room isn't open) take a permit from a
  `Semaphore` sized `std::thread::available_parallelism()` for the whole parse and closure.
  Concurrent cold reads beyond that wait; memory is bounded by permits × room size.
  `→ skipped: sharing one parse between concurrent readers of the same room; add when
  catalogue reads of big rooms get frequent.`
- `WebSocketUpgrade::max_message_size(16 MiB)` and `max_frame_size(16 MiB)` (uvicorn's
  default `ws_max_size`, which the Python server ran with), as one named constant.
- [ ] Tests: `cold_reads_are_bounded` (with the semaphore sized 1 through a test
  constructor: two concurrent reads never overlap — record enter/leave in the closure),
  `oversized_message_closes_the_socket` (16 MiB + 1 → close 1009),
  `sixteen_mib_update_is_accepted`.
- [ ] Red, implement, gate. README: the limit in the close codes table. Commit
  "Bound cold reads and websocket messages".

### Task 5: Check rights before looking a member up (finding 1-05)

**Files:** `ui/room.rs` (`set_role`, `remove`, `transfer`), `ui/groups.rs`
(`remove_member`). Test: `tests/ui_rooms.rs`, `tests/ui_groups.rs`.
- The caller's role on the room or group is checked before `member_email`; a caller who
  may not act gets the same answer whether the id exists or not.
- [ ] Test: `unknown_and_existing_ids_answer_alike` for each of the four routes (stranger
  and viewer callers).
- [ ] Red, implement, gate. Commit "Check rights before looking a member up".

### Task 6: Persist only what changed (finding 3-02)

**Files:** `room.rs` (`handle_update`). Test: `room.rs` tests.
- Today the incoming update bytes are appended to the `.y` file and broadcast even when
  applying them changes nothing (a repeated 4 MB update grows the file ~300 MB/s while the
  document stays small). Append and broadcast the change yrs itself reports for the
  transaction (`observe_update_v1` on the room's doc, or the transaction's
  `encode_update_v1` diff), and nothing when it reports none.
- [ ] Tests: `repeated_update_appends_nothing` (apply the same update twice: one record in
  the file, one broadcast), `delete_only_update_is_kept` (an update that only deletes is
  persisted and survives a reopen), and the existing file-parity tests still pass.
- [ ] Red, implement, gate. Run `bench/correctness/file_parity.py` and
  `wire_parity.py` once and paste PASS. Commit "Store only what an update changes".

### Task 7: Storage limits per room and per owner (findings 3-02, 1-02 follow-up)

User decision (2026-11-06): at most ~1 GB per room, and a limit per user. A user's usage is
the total `.y` size of the rooms they **own** (editors' writes count against the owner, like
a shared drive). Default 5 GB per owner (my default, user can change it).

**Files:** `room.rs`, `cli.rs` (env), `lib.rs`/`AppState` if the limits travel there,
`db.rs` (rooms owned by a user). Test: `room.rs` tests, `tests/websocket.rs`.
- Env: `COCAT_ROOM_MAX_MB` (default 1024) and `COCAT_OWNER_MAX_MB` (default 5120); same
  parsing rules as `COCAT_ROOM_MEMORY_BUDGET` (positive integer, else startup error naming
  the variable; `0` is not allowed; unset/empty = default).
- When a room opens, the room thread gets an **allowance** in bytes: `min(room max,
  owner max − the size of the owner's other room files)`. `handle_update` refuses an update
  whose append would take the file past the allowance: nothing applied, nothing appended,
  nothing broadcast, and the sending client is closed with a new `CloseReason::Full`, code
  **4507**, reason "room storage limit reached". Other clients stay connected. The refusal
  is logged at `warn` with the room id.
  `simplify:` the allowance is computed at open; two of one owner's rooms open at once can
  each use the remaining quota (overshoot ≤ remaining × open rooms). Upgrade path: one
  shared per-owner byte counter in the `RoomManager`.
- Cold reads (Task 4's semaphore): a fixed **2** permits instead of one per core (user
  decision: these reads are rare; SciQLop syncs over the websocket). One named constant.
- [ ] Tests: `update_past_the_room_limit_is_refused` (tiny limit through a test
  constructor; the file and the doc are unchanged, the sender gets 4507, another client
  stays connected and still receives a later small update... from an editor under the
  limit, if the limit allows it — pick sizes so it does), `owner_quota_counts_other_rooms`
  (two rooms of one owner, the second's allowance is reduced by the first's file size),
  `limits_from_env` (defaults, a bad value is a startup error naming the variable).
- [ ] Red, implement, gate. README: the two variables in the environment table, 4507 in
  the close codes table. Commit "Limit room storage per room and per owner".

### Task 8: Report

- [ ] `docs/superpowers/reports/2026-11-06-phase-4b.md`: the models and their run times,
  every finding (who found it, reproducer test, fix commit), the attackers' "did not work"
  lists condensed, the unproven leads kept for later (disk quota per room, catalogue export
  blocking an open room). Commit "Report the attacker campaign".
