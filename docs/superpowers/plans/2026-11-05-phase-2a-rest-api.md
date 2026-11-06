# Phase 2a — REST API for rooms, members and invitations — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serve the Python server's `/api` (rooms, members, invitations, `/api/me`), the
two legacy routes and the login rate limit, so the room part of the
Python `test_routes.py` passes against the Rust binary.

**Architecture:** One `api` module holds the routes and the request/response types; it
calls `lifecycle` (phase 1) for every change that revokes access, and a new `invitations`
module for the invitation rules. A `CurrentUser` extractor turns the cookie into a user or a
401. Errors map to `{"detail": "..."}` with the status table below.

**Tech Stack:** axum 0.8 (json), sqlx 0.9, serde/serde_json; new crates: `sha2` 0.10,
`base64` 0.22, `getrandom` 0.3, `email_address` 0.2.

**Spec:** `docs/superpowers/specs/2026-11-05-cocat-rs-design.md` ("The HTTP and websocket
contract", phase 2). Behaviour reference: `../cocat-python-tests` branch
`rust-spike`: `src/cocat/app/{routes.py,rooms.py,invitations.py,actions.py,schemas.py,
ratelimit.py,prefix.py,app.py}` and `tests/test_routes.py`.

**Not in 2a** (own plans): 2b accounts — register, verify, forgot/reset password, the
personal room on register, `/users/*`, logout, SMTP (`lettre`), ORCID (`openidconnect`),
`COCAT_PROXY_PREFIX` (the proxy strips the prefix before Rust sees the request, so routing
needs nothing; the prefix only matters for URLs the server generates, which start in 2b).
2c — the read-only catalogue API. Until 2b, an invitation with an email returns its token,
which is what Python does without a mailer.

## Global Constraints

- Worktree `../cocat-rs-p1`, branch `phase-1` (2a continues it).
  Never touch the `cocat-rs` main checkout.
- Rules file and gate as in phase 1 (`rust-rules.md`): TDD, one cargo command at a time,
  fmt + clippy `-D warnings` + `cargo test --workspace` before every commit.
- Error body is always `{"detail": "<message>"}`. Status table (from `routes.py`
  `ERROR_STATUS` and FastAPI): `NotFound` 404, `Forbidden` 403, `Conflict` 409, other room
  errors 400, a body that does not match the schema 422, no or bad cookie 401 with detail
  `"Unauthorized"`, rate limited 429.
- Non-members get 404 on every room route, the same as a missing room.
- Messages are copied verbatim from Python where a test reads them (`rooms.py`,
  `invitations.py`).
- Datetimes in responses: RFC 3339 UTC with a `Z`, e.g. `2026-11-19T14:03:07.123Z`
  (SQLite `strftime('%Y-%m-%dT%H:%M:%fZ', ...)`; no date crate).

## Review Focus

1. **An invitation token used twice at the same time.** Two accepts of a `max_uses = 1`
   token racing. Expected: exactly one joins, the other gets 404 (Task 3 test
   `a_single_use_invitation_admits_one_of_two_racing_accepts`: the consume is one
   conditional `UPDATE ... WHERE uses < max_uses`, never read-then-write).
2. **A token in a URL or a log.** Expected: tokens only travel in JSON bodies, are stored
   as SHA-256 hex, never logged (Task 3 test `the_database_stores_only_the_token_hash`).
3. **Accepting an invitation that lowers a role.** An owner accepts a viewer invitation to
   their own room. Expected: they stay owner (Task 3 test
   `accepting_a_lower_invitation_keeps_the_higher_role`).
4. **X-Forwarded-For spoofing.** A client sends its own `X-Forwarded-For` with no trusted
   proxy configured. Expected: the limiter keys on the socket address, so spoofing does not
   bypass the limit (Task 4 test `forwarded_for_is_ignored_without_trusted_proxies`).
5. **A room name with control characters or only spaces.** Expected: 400 with Python's
   messages, and a name outside 1..200 characters is 422 (Task 2 tests
   `a_blank_room_name_is_a_bad_request`, `a_room_name_with_control_characters_is_refused`).

---

### Task 1: API scaffolding, `/api/me`, legacy routes

**Files:**
- Create: `crates/cocat-server/src/api.rs`
- Modify: `crates/cocat-server/src/ws.rs` (router), `crates/cocat-server/src/lib.rs`,
  `crates/cocat-server/src/db.rs` (`list_user_rooms`, `list_members`)
- Test: `crates/cocat-server/tests/api.rs` (new; an in-process server fixture with a
  dev-dependency `tower` 0.5 with the `util` feature, for `ServiceExt::oneshot` on the
  router; no HTTP client crate)

**Interfaces:**
- Produces: `api::router() -> Router<AppState>` mounted at `/api`; `api::CurrentUser(pub
  db::User)` extractor (401 `{"detail":"Unauthorized"}` when the cookie is missing, bad,
  or the user inactive); `api::ApiError` implementing `IntoResponse` with
  `From<LifecycleError>`, `From<DbError>` (500, logged, generic detail) and the table above.
- Produces: `db::list_user_rooms(pool, user_id) -> Result<Vec<(RoomRow, Role)>, DbError>`
  ordered by name then id; `db::list_members(pool, room_id) -> Result<Vec<(db::User, Role)>,
  DbError>` ordered by email; `pub struct RoomRow { pub id: String, pub name: String }`.
- Routes: `GET /api/me` → `{"id","email","is_active","is_superuser","is_verified"}`;
  `GET /rooms` → `{"rooms": [ids]}`; `GET /room/{id}/users` → `{"users": [emails]}`
  (viewer or better, else 404).

- [ ] **Step 1: Failing tests:** `me_without_a_cookie_is_401`, `me_returns_the_user`,
  `legacy_rooms_lists_room_ids`, `legacy_room_users_needs_membership` (non-member 404),
  `a_malformed_json_body_is_422` (on any POST route added later; here use a test-only
  route only if none exists yet, otherwise move this test to Task 2).
- [ ] **Step 2: Run them**, record the first failure.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Full gate.**
- [ ] **Step 5: Commit** "Add the API scaffolding and the legacy routes".

### Task 2: Rooms and members

**Files:**
- Modify: `crates/cocat-server/src/api.rs`, `crates/cocat-server/src/db.rs`
  (`rename_room`), `crates/cocat-server/src/lifecycle.rs` (`create_room`, `rename_room`)
- Test: `crates/cocat-server/tests/api.rs`

**Interfaces:**
- Consumes: phase 1 `lifecycle::{delete_room, set_member_role, remove_member,
  transfer_ownership}`, Task 1 `CurrentUser`, `ApiError`.
- Produces: `lifecycle::create_room(state, owner: Uuid, name: &str) -> Result<RoomRow,
  LifecycleError>` (id = `Uuid::new_v4().simple()`), `lifecycle::rename_room(state,
  room_id, actor, name) -> Result<RoomRow, LifecycleError>` (owner); `LifecycleError::Invalid
  (&'static str)` → 400; name rule from `rooms.py::_room_name`: trim, empty →
  `"Enter a room name"`, any char `< ' '` or `'\x7f'` →
  `"Enter a room name without control characters"`; JSON `name` length 1..=200 else 422.
- Routes (all JSON, statuses from `routes.py`): `GET /api/rooms` → `[{"id","name","role"}]`;
  `POST /api/rooms {"name"}` → 201 `{"id","name","role":"owner"}`;
  `PATCH /api/rooms/{id} {"name"}` → `{"id","name","role":"owner"}`;
  `DELETE /api/rooms/{id}` → 204; `GET /api/rooms/{id}/members` →
  `[{"user_id","email","role"}]` (viewer or better); `PATCH /api/rooms/{id}/members/{uid}
  {"role":"viewer"|"editor"}` → 204 (any other role value 422);
  `DELETE /api/rooms/{id}/members/{uid}` → 204; `POST /api/rooms/{id}/owner {"user_id"}` → 204.

- [ ] **Step 1: Failing tests**, ported from `test_routes.py`:
  `create_list_rename_delete_room`, `member_management` (list, role change, removal,
  transfer), `errors_map_to_status` (404 non-member, 403 editor acting as owner, 409 owner
  role, 400 blank name), `a_blank_room_name_is_a_bad_request`,
  `a_room_name_with_control_characters_is_refused`, `a_too_long_room_name_is_422`,
  `the_owner_can_assign_the_viewer_role`, `the_owner_role_is_not_assignable_over_http`,
  `deleting_over_http_closes_the_sockets_and_the_file` (one live socket, 1008).
- [ ] **Step 2: Run them**, record the first failure.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Full gate.**
- [ ] **Step 5: Commit** "Serve rooms and members over the API".

### Task 3: Invitations

**Files:**
- Create: `crates/cocat-server/migrations/0002_invitations.sql`,
  `crates/cocat-server/src/invitations.rs`
- Modify: `crates/cocat-server/src/api.rs`, `crates/cocat-server/Cargo.toml` (`sha2`,
  `base64`, `getrandom`, `email_address`)
- Test: `crates/cocat-server/tests/invitations.rs` (new), `crates/cocat-server/tests/api.rs`

**Interfaces:**
- Table `invitation` as Python's `db.py::Invitation`: id INTEGER PK, token_hash TEXT
  UNIQUE NOT NULL, room_id → room ON DELETE CASCADE, role CHECK editor/viewer, email
  TEXT NULL, created_by → user ON DELETE SET NULL, created_at, expires_at, revoked BOOL
  default 0, max_uses INTEGER NULL, uses INTEGER default 0.
- Produces (`invitations.rs`): `create(state, room_id, actor, role, email: Option<&str>,
  expires_in_days: u32, max_uses: Option<u32>) -> Result<(i64, String), LifecycleError>`
  (owner; `Owner` role → `Conflict("An invitation cannot grant the owner role")`; email →
  `max_uses = 1`; token = 32 random bytes, base64 URL-safe no padding, stored as SHA-256
  hex); `preview(state, token) -> Result<Preview, LifecycleError>`; `accept(state, token,
  user: &db::User) -> Result<(RoomRow, Role), LifecycleError>` (email-bound invitation for
  another address → `Forbidden("This invitation is for another email address")`, compared
  case-insensitively; consume with one conditional UPDATE; `db::add_member` (raise-only);
  then `state.rooms.revoke(room_id, user)` like Python's `RoomActions.accept_invitation`);
  `list(state, room_id, actor)` (owner, valid only, newest first); `revoke(state, room_id,
  actor, invitation_id)`. A valid invitation: not revoked, `expires_at > now`, `max_uses`
  null or `uses < max_uses`. Invalid or unknown token → `NotFound("Invitation not found")`.
- Email invitation: trim; empty, invalid (`email_address`), or a placeholder address
  (Python `mail.py::is_placeholder`, copy its exact rule; the domain is
  `placeholder.cocat.org`) →
  `Invalid("Enter a valid email address")`.
- Routes: `POST /api/rooms/{id}/invitations {"email"?, "role"="editor", "expires_in_days"=14
  (1..=90), "max_uses"? (>=1)}` → 201 `{"id","token"}`; `GET /api/rooms/{id}/invitations` →
  `[{"id","email","role","expires_at","uses","max_uses"}]`;
  `DELETE /api/rooms/{id}/invitations/{iid}` → 204; `POST /api/invitations/preview
  {"token"}` (no auth) → `{"room_id","room_name","role","inviter_email","expires_at"}`;
  `POST /api/invitations/accept {"token"}` → `{"id","name","role"}`.

- [ ] **Step 1: Failing tests:** `invite_link_flow` (create, preview, accept, member listed),
  `an_invitation_can_grant_the_viewer_role`, `an_invitation_cannot_grant_owner`,
  `an_expired_invitation_is_not_found`, `a_revoked_invitation_is_not_found`,
  `an_email_invitation_is_bound_to_that_address` (other user 403, case-insensitive match
  accepted), `a_single_use_invitation_admits_one_of_two_racing_accepts` (two concurrent
  accepts via `tokio::join!`, exactly one Ok),
  `accepting_a_lower_invitation_keeps_the_higher_role`,
  `accepting_a_higher_invitation_closes_the_old_socket` (live socket, 1008),
  `the_database_stores_only_the_token_hash`, `a_placeholder_address_is_refused`.
- [ ] **Step 2: Run them**, record the first failure.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Full gate.**
- [ ] **Step 5: Commit** "Add invitations".

### Task 4: Login rate limit

**Files:**
- Create: `crates/cocat-server/src/ratelimit.rs`
- Modify: `crates/cocat-server/src/ws.rs` (router), `crates/cocat-server/src/cli.rs`
  (env), `crates/cocat-server/src/lib.rs` (`AppState` gains the limiter), `README.md`
- Test: `crates/cocat-server/tests/ratelimit.rs` (new)

**Interfaces:**
- Produces: `RateLimiter::new(max_calls: u32, period: Duration, max_keys: usize)`,
  `RateLimiter::hit(&self, key: &str, now: Instant) -> bool` (fixed window per key; at
  `max_keys`, first drop expired windows, then the oldest; a `Mutex<HashMap>` inside),
  `client_key(headers: &HeaderMap, peer: SocketAddr, trusted_proxies: usize) -> String`
  (Python `ratelimit.py::client_key`: with N trusted proxies and at least N hops, the N-th
  hop from the right, else the peer IP). Limits from the spec: 10 calls / 60 s, key
  `path|client`, 10 000 keys, plain 429. Applied to `POST /auth/jwt/login` now (2b adds
  register, forgot-password, request-verify-token).
- `COCAT_TRUSTED_PROXIES`: unset/empty → 0; not a non-negative integer → startup error
  naming the variable. The peer address needs
  `into_make_service_with_connect_info::<SocketAddr>()`.

- [ ] **Step 1: Failing tests:** `the_eleventh_login_in_a_minute_is_429`,
  `the_window_resets_after_the_period` (drive `hit` with explicit `Instant`s, no sleeps),
  `routes_have_separate_budgets`, `forwarded_for_is_ignored_without_trusted_proxies`,
  `the_trusted_hop_is_the_key`, `the_key_table_is_bounded`,
  `a_malformed_trusted_proxies_is_a_startup_error`.
- [ ] **Step 2: Run them**, record the first failure.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Full gate.**
- [ ] **Step 5: Commit** "Rate-limit login".

### Task 5: Grow the Python live slice

**Files (cocat-python-tests, branch `rust-spike`):**
- Modify: `tests/conftest.py` (replace `LIVE_SLICE` with `LIVE_SKIP`: a dict test name →
  reason), `tests/test_live.py`

Expected `LIVE_SKIP` (verify each by running; keep a reason per entry):
`test_new_user_gets_personal_room_not_email_prefix` and
`test_a_placeholder_address_cannot_register` (register, 2b),
`test_an_invitation_email_is_sent` and `test_a_mailer_needs_a_public_url` (SMTP, 2b),
`test_websocket_does_not_hold_a_db_connection` and
`test_a_downgrade_during_the_handshake_is_honoured` (read Python internals; their Rust
equivalents exist). Everything else in `test_routes.py` must pass live.

- [ ] **Step 1:** Release build in the worktree; run the live slice
  (`COCAT_SERVER_CMD=.../cocat-rs-p1/target/release/cocat-server pytest tests/test_routes.py
  tests/test_server.py -q`) and fix *Rust* until every non-skipped test passes. A test that
  fails on a Python-specific detail the contract does not need gets a skip entry with that
  reason, reported for the lead's ruling, never a silent skip.
- [ ] **Step 2:** Normal suite unchanged: `coverage run -m pytest tests` +
  `coverage report --fail-under=100`, `mypy src tests`, `ruff check`, `ruff format --check`.
- [ ] **Step 3:** One commit in each repo that changed. Do not push.
