# Phase 2d — User groups — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users gather people into groups and share a room with a whole group in one
step, with access taken away immediately when someone leaves the group or the group loses
the room.

**Architecture:** Three new tables (groups, group members, room-to-group grants) and one
new idea in `db`: a user's *effective* role in a room is the highest of their direct
membership and every grant to a group they belong to. Every access check already goes
through `db::role_in`, so it becomes the effective role; member management keeps using
the direct membership (`db::direct_role`). A `groups` module holds the rules and a
`groups_api` module the routes, like `invitations` / `invitations_api`. Group invitations
reuse the room invitations' token helpers.

**Tech Stack:** existing crates only.

**Spec:** the user's decision of 2026-11-05 (no spec document): groups of users so the
same set of people can be given several rooms (and, later, workspaces) at once.
Agreed model: a group has one owner and members; any user can create one; the owner adds
people by invitation; a room is shared with a group as editor or viewer, never owner;
a user's role is the highest of their grants; no nested groups; losing access through a
group closes the user's sockets in every room it reached, after the change commits.

## Global Constraints

- Repo `/home/jeandet/Documents/prog/cocat-rs`, branch `main`, commit per task, never push.
  Rules file, TDD, and the full gate after the *last* edit of each task:
  `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --workspace`.
  One build or test command at a time, in the foreground.
- **Commit, then revoke**, as everywhere else: a revoke runs only after the write that
  took the access away has committed. A fan-out (many users × many rooms) is collected
  *before* the write when the write cascades it away.
- A group grant can never give `owner`. Every owner-only check (`require_role(..,
  Role::Owner)`) is unchanged and, since owner only exists as a direct membership, stays
  correct with the effective role.
- Group visibility: a user who is not a member of a group gets 404 `"Group {id} not
  found"` for every route about it, like rooms. A member who isn't the owner gets 403 on
  owner-only routes.
- Error bodies `{"detail": ...}`; names 1..=200 characters (reuse the room-name rule).
- SQLite table names: `user_group`, `group_member`, `room_group` (`group` is an SQL keyword).

## Review Focus

1. **Removed from a group while a socket is attaching.** Expect: the bridge's post-attach
   role re-read (which now reads the effective role) closes it with 1008. → Task 5 test.
2. **A direct member removed from a room who still has a group grant to it.** Expect: they
   keep access at the group's role; their sockets are closed once (they re-attach with the
   new role); `GET /api/rooms` still lists the room with the group role. → Task 1 test.
3. **An editor-through-a-group** tries owner actions (delete, invite, grant to a group,
   change roles). Expect: 403, exactly like a direct editor. → Task 3 test.
4. **Granting a room to a group the actor isn't in.** Expect: 404 for the group (no
   enumeration of group ids), even if the group exists. → Task 3 test.
5. **Deleting a group, or a user, that reaches many rooms.** Expect: every (member, room)
   pair revoked after the commit, none missed because the cascade already removed the
   rows. → Task 2 test (group) and Task 1 test (user).

---

### Task 1: Schema and the effective role

**Files:**
- Create: `crates/cocat-server/migrations/0003_groups.sql`
- Modify: `crates/cocat-server/src/db.rs`, `crates/cocat-server/src/lifecycle.rs`
- Test: `crates/cocat-server/tests/groups_db.rs` (new)

**Interfaces:**
- Schema:
  - `user_group(id TEXT PRIMARY KEY, name TEXT NOT NULL, created_at DATETIME NOT NULL
    DEFAULT CURRENT_TIMESTAMP, created_by TEXT REFERENCES "user"(id) ON DELETE SET NULL)`.
  - `group_member(group_id TEXT NOT NULL REFERENCES user_group(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES "user"(id) ON DELETE CASCADE, role TEXT NOT NULL CHECK
    (role IN ('owner','member')), joined_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (group_id, user_id))`, plus `CREATE UNIQUE INDEX one_owner_per_group ON
    group_member(group_id) WHERE role = 'owner'` and an index on `group_member(user_id)`.
  - `room_group(room_id TEXT NOT NULL REFERENCES room(id) ON DELETE CASCADE, group_id TEXT
    NOT NULL REFERENCES user_group(id) ON DELETE CASCADE, role TEXT NOT NULL CHECK (role IN
    ('editor','viewer')), PRIMARY KEY (room_id, group_id))`, plus an index on
    `room_group(group_id)`.
- Produces in `db.rs`:
  - `pub async fn direct_role(pool, room_id, user_id) -> Result<Option<Role>, DbError>`:
    today's `role_in`, renamed.
  - `pub async fn role_in(pool, room_id, user_id) -> Result<Option<Role>, DbError>`: the
    effective role, one query (`UNION ALL` of the direct row and the group grants), the
    highest `Role::rank` wins.
  - `list_user_rooms` returns every room the user reaches, directly or through groups, one
    row per room with the effective role, same order as today.
  - `list_members` is unchanged (direct members only).
- `lifecycle.rs`: `member_role` (used to manage a *member*: set role, remove, transfer
  ownership) switches to `direct_role`. `require_role` (checks the *actor*) keeps `role_in`.

- [ ] **Step 1: Failing tests** (`groups_db.rs`, inserting group rows with plain SQL since
  the group functions come in Task 2):
  - `group_grant_gives_access`: a user only in a group granted `viewer` → `role_in` is
    `Viewer`, `direct_role` is `None`.
  - `highest_role_wins`: direct `viewer` + group `editor` → `Editor`; direct `editor` +
    group `viewer` → `Editor`; owner + any group grant → `Owner`.
  - `rooms_through_groups_are_listed_once`: a room reached directly and through two groups
    appears once, with the highest role.
  - `group_delete_cascades_grants`: deleting the `user_group` row removes its
    `room_group` and `group_member` rows; `role_in` falls back to the direct role.
  - `removing_a_direct_member_keeps_group_access` (review focus 2): through
    `lifecycle::remove_member`, the user's `role_in` becomes the group role and
    `GET /api/rooms` still lists the room.
  - `deleting_a_user_revokes_group_rooms` (review focus 5): a user with an open socket in a
    room reached only through a group; `accounts::delete` closes it with 1008.
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Gate, expect PASS** (every existing test too: nothing about direct members
  changes). **Commit** "Groups schema; a room role is the highest grant".

### Task 2: Groups and their members

**Files:**
- Create: `crates/cocat-server/src/groups.rs`, `crates/cocat-server/src/groups_api.rs`
- Modify: `crates/cocat-server/src/lib.rs`, `crates/cocat-server/src/api.rs` (merge the
  router; make the room-name type and `AssignableRole` `pub(crate)` for reuse)
- Test: `crates/cocat-server/tests/groups_api.rs` (new)

**Interfaces:**
- `groups.rs` (errors are `LifecycleError`, mapped by the existing `From`):
  - `pub async fn create(state, actor, name) -> Result<GroupRow, LifecycleError>`: new uuid
    id, actor becomes `owner`, one transaction.
  - `pub async fn list_for(state, user) -> Result<Vec<(GroupRow, GroupRole)>, ..>`
    sorted by (name, id).
  - `pub async fn rename(state, group_id, actor, name)` (owner).
  - `pub async fn delete(state, group_id, actor)` (owner): collect the (member, room)
    pairs, delete, commit, revoke every pair.
  - `pub async fn members(state, group_id, actor) -> Vec<(User, GroupRole)>` (any member).
  - `pub async fn remove_member(state, group_id, actor, member)`: owner removes others,
    anyone leaves; the owner cannot leave (409 `"The owner cannot leave the group"`; delete
    it instead). Commit, then revoke the member in every room granted to the group.
  - `pub struct GroupRow { pub id: String, pub name: String }`, `pub enum GroupRole
    { Owner, Member }` with `as_str`.
  - A private `revoke_pairs(state, pairs: Vec<(String, Uuid)>)` shared by every fan-out in
    this module and Task 3's.
- Routes (all under `/api`, cookie required):
  - `GET /groups` → `{"groups": [{"id","name","role"}]}`
  - `POST /groups` `{"name"}` → 201 `{"id","name","role":"owner"}`
  - `PATCH /groups/{group_id}` `{"name"}` → 200 group; `DELETE /groups/{group_id}` → 204
  - `GET /groups/{group_id}/members` → `{"members": [{"user_id","email","role"}]}`
  - `DELETE /groups/{group_id}/members/{user_id}` → 204
- `// simplify:` no ownership transfer for groups yet; an owner who wants out deletes the
  group. Add `POST /groups/{id}/owner` when someone asks.

- [ ] **Step 1: Failing tests:** `create_and_list`, `name_length_is_422`,
  `non_member_gets_404_everywhere` (all five routes), `member_cannot_rename_or_delete_403`,
  `members_listed_by_email`, `member_can_leave`, `owner_cannot_leave_409`,
  `owner_removes_member_and_their_sockets_close` (socket in a room granted to the group
  via SQL → 1008), `delete_group_revokes_every_member_in_every_room` (review focus 5: two
  rooms, three members, all six sockets close), `anonymous_401`.
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Gate, expect PASS. Commit** "Create groups and manage their members".

### Task 3: Share a room with a group

**Files:**
- Modify: `crates/cocat-server/src/groups.rs`, `crates/cocat-server/src/groups_api.rs`
- Test: `crates/cocat-server/tests/groups_api.rs`

**Interfaces:**
- `pub async fn grant(state, room_id, group_id, actor, role: Role)`: actor must be the
  room's owner (`require_role(.., Owner)`) **and** a member of the group (else 404 for the
  group, review focus 4); `role` is `Editor` or `Viewer` (the route deserializes
  `AssignableRole`). Upsert. When it *changes* an existing grant, revoke every group
  member in that room after the commit (their sockets carry the old role).
- `pub async fn ungrant(state, room_id, group_id, actor)`: room owner only (the actor need
  not be in the group any more); delete, commit, revoke every group member in the room.
- `pub async fn room_groups(state, room_id, actor) -> Vec<(GroupRow, Role)>`: any room member.
- Routes: `GET /rooms/{room_id}/groups` → `{"groups": [{"id","name","role"}]}`;
  `PUT /rooms/{room_id}/groups/{group_id}` `{"role"}` → 200 the grant;
  `DELETE /rooms/{room_id}/groups/{group_id}` → 204 (404 when there was no grant).

- [ ] **Step 1: Failing tests:** `owner_grants_and_members_get_access` (a group member's
  `GET /api/rooms` now lists the room with the granted role, and `/room/{id}` accepts them),
  `grant_to_a_foreign_group_is_404` (the group exists, the actor isn't in it),
  `editor_through_group_cannot_do_owner_things` (review focus 3: delete room, create an
  invitation, change a member's role, grant to a group → 403 each),
  `owner_role_cannot_be_granted_422`, `changing_a_grant_revokes_the_group_in_that_room`,
  `ungrant_revokes`, `ungrant_unknown_is_404`, `room_groups_visible_to_viewers`.
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Gate, expect PASS. Commit** "Share a room with a group".

### Task 4: Group invitations

**Files:**
- Create: `crates/cocat-server/migrations/0004_group_invitations.sql`
- Modify: `crates/cocat-server/src/invitations.rs` (make the token helpers `pub(crate)`),
  `groups.rs`, `groups_api.rs`, `crates/cocat-server/src/links.rs`
- Test: `crates/cocat-server/tests/groups_api.rs`

**Interfaces:**
- Table `group_invitation`: same columns as `invitation`, with `group_id TEXT NOT NULL
  REFERENCES user_group(id) ON DELETE CASCADE` in place of `room_id` and no `role` column
  (everyone joins as `member`).
- Same rules as room invitations, reusing their helpers (token = 32 random bytes,
  base64url; stored as SHA-256 hex; expiry; `max_uses`; an addressed invitation is
  single-use and only for that email; accepting is one conditional UPDATE inside one
  transaction; mailed when an email is given and a mailer is configured, the token then
  not returned).
- Link: `links.group_invitation(token)` → `{public}{prefix}/ui/groups/join/{token}` (the
  page comes in phase 3).
- Routes: `POST /groups/{group_id}/invitations` (owner) `{"email"?, "expires_in_days",
  "max_uses"?}` → 201, same body shape as room invitations; `GET
  /groups/{group_id}/invitations` (owner); `DELETE /groups/{group_id}/invitations/{id}`
  (owner); `POST /group-invitations/preview` `{"token"}`; `POST
  /group-invitations/accept` `{"token"}` → 200 `{"id","name","role":"member"}`. Accepting
  when already a member is 200 and spends no use. After a new member joins, revoke them in
  the group's rooms (their sockets there were admitted under their old role, as room
  invitations do).

- [ ] **Step 1: Failing tests:** mirror the room-invitation tests that apply
  (`accept_joins_as_member`, `addressed_invitation_other_email_403`, `expired_is_404`,
  `revoked_is_404`, `max_uses_spent_is_404`, `concurrent_accepts_respect_max_uses`,
  `already_member_spends_no_use`, `mailed_invitation_hides_the_token`,
  `link_uses_public_url`), plus `non_owner_cannot_invite_403`.
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Gate, expect PASS. Commit** "Invite people into a group".

### Task 5: End to end, and docs

**Files:**
- Test: `crates/cocat-server/tests/groups_ws.rs` (new); `README.md`

- [ ] **Step 1: Tests over real sockets** (same harness as the existing websocket tests):
  `group_member_syncs_the_room` (an update from a group editor reaches a direct member);
  `group_viewer_writes_are_dropped`; `removed_from_group_mid_handshake_is_closed`
  (review focus 1: remove the member between the HTTP upgrade and the attach, using the
  existing `before_attach` hook; the socket closes 1008).
- [ ] **Step 2: Run them.** They should pass if Tasks 1–4 are right; a failure here is a
  bug in those tasks: fix it there, with its own red test.
- [ ] **Step 3: README:** a "Groups" section: the model in five lines, the routes, and the
  rule that a direct removal doesn't remove group access. Spec status line: phase 2d done.
- [ ] **Step 4: Gate, expect PASS. Commit** "Test groups over sockets and document them".
