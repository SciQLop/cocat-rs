# Phase 7a — Administration dashboard — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A superuser-only `/ui/admin` section (overview, users, rooms) with per-user and
per-room storage quotas.

**Architecture:** Task 1 (server, opencode): migration 0007 for the two quota columns, the
allowance reading them, a read model `admin.rs` (overview numbers, user rows, room rows)
and the admin actions as functions over the existing lifecycle/accounts code. Task 2 (UI,
done by the reviewer directly, with screenshots): askama pages and form handlers under
`/ui/admin`, the header link.

**Tech Stack:** existing: axum, sqlx/SQLite, askama, our stylesheet. No new crate.

**Spec:** `docs/superpowers/specs/2026-11-06-admin-dashboard-design.md` (approved
2026-11-06).

## Global Constraints

- Repo `cocat-rs`, branch `main`, commit per task, never push. ONE cargo command at a
  time, foreground. TDD. Full gate before each commit.
- Every admin function takes the acting `User` and returns `NotFound` unless
  `is_superuser` (the UI maps it to 404). Self-protection: a superuser cannot deactivate,
  delete or remove superuser from themselves (`AdminError::OwnAccount`).
- Quotas in MB, `NULL` = the environment default, `0` refused (`ValueError`-style
  `AdminError::InvalidQuota`), max 1 048 576 MB.
- Reuse: `accounts::change` for active/verified/superuser (it already ends sessions on
  deactivation; check), `accounts::delete`, `lifecycle::delete_room`,
  `lifecycle::transfer_ownership` (or the `db::set_owner` path the CLI uses, whichever
  closes sockets the same way the room page does). No second copy of their side effects.

## Review Focus

1. A non-superuser (or anonymous caller) reaching any admin route or action: 404, nothing
   changed. Access matrix rows for every route.
2. A quota override that the allowance ignores (room and owner, set and cleared).
3. Self-lockout: the last action a superuser could take against themselves.
4. Room ids with spaces/dots (imported from Python) in admin URLs and forms.
5. The overview on a server with rooms in the DB whose file does not exist yet (size 0).

---

### Task 1: Quotas and the admin read model and actions (server)

**Files:** `migrations/0007_quotas.sql`, `src/room.rs` (allowance), `src/admin.rs` (new),
`src/db.rs` (queries), `src/room.rs` (client counts accessor), tests `tests/admin.rs`,
`tests/quotas.rs`.

**Interfaces (Produces):**
```rust
pub struct Overview { pub users: UserCounts, pub rooms: RoomCounts, pub storage: StorageSummary, pub server: ServerInfo }
pub struct UserCounts { pub total: u64, pub active: u64, pub unverified: u64, pub superusers: u64, pub recent: u64 }
pub struct RoomCounts { pub total: u64, pub open: u64, pub clients: u64 }
pub struct StorageSummary { pub total_bytes: u64, pub largest: Vec<RoomRow>, pub owners_near_quota: Vec<UserRow> }
pub struct ServerInfo { pub version: &'static str, pub uptime_secs: u64, pub resident_bytes: Option<u64>, pub room_limit_bytes: u64, pub owner_limit_bytes: u64 }
pub struct UserRow { pub id: Uuid, pub email: String, pub created_at: String, pub is_active: bool, pub is_verified: bool, pub is_superuser: bool, pub rooms_owned: u64, pub used_bytes: u64, pub quota_bytes: u64, pub quota_override_mb: Option<u64> }
pub struct RoomRow { pub id: String, pub name: String, pub owner: Option<String>, pub size_bytes: u64, pub limit_bytes: u64, pub limit_override_mb: Option<u64>, pub members: u64, pub open: bool, pub clients: u64 }
pub async fn overview(state: &AppState, actor: &User) -> Result<Overview, AdminError>;
pub async fn users(state: &AppState, actor: &User, search: Option<&str>) -> Result<Vec<UserRow>, AdminError>;
pub async fn rooms(state: &AppState, actor: &User) -> Result<Vec<RoomRow>, AdminError>; // largest first
pub async fn set_user_flags(state: &AppState, actor: &User, user: Uuid, active: Option<bool>, verified: Option<bool>, superuser: Option<bool>) -> Result<(), AdminError>;
pub async fn delete_user(state: &AppState, actor: &User, user: Uuid) -> Result<(), AdminError>;
pub async fn set_user_quota(state: &AppState, actor: &User, user: Uuid, mb: Option<u64>) -> Result<(), AdminError>;
pub async fn set_room_limit(state: &AppState, actor: &User, room: &str, mb: Option<u64>) -> Result<(), AdminError>;
pub async fn set_room_owner(state: &AppState, actor: &User, room: &str, owner_email: &str) -> Result<(), AdminError>;
pub async fn delete_room(state: &AppState, actor: &User, room: &str) -> Result<(), AdminError>;
pub enum AdminError { NotFound, OwnAccount, InvalidQuota, UnknownUser, Db(..), Account(..), Room(..) }
```
Server start time: record once in `AppState` (or a `LazyLock<Instant>` set in `serve`).

- [ ] Tests first (`tests/quotas.rs`): a room limit override caps `allowance`; an owner
  quota override caps it; clearing returns to the env default; 0 refused.
- [ ] Tests first (`tests/admin.rs`): non-superuser gets `NotFound` from every function and
  nothing changes; each action does what it says through the existing paths (deactivate
  ends sessions; delete room removes the file and closes sockets; owner change); own-
  account protection; overview counts on a seeded fixture (including a room whose file
  does not exist: size 0); search filters by email substring, case-insensitive; a room id
  with a space round-trips.
- [ ] Implement, gate, commit "Add quotas and the admin model".

### Task 2: Admin pages (UI, reviewer)

**Files:** `src/ui/admin.rs`, `templates/admin_*.html`, `templates/base.html` (Admin link
for superusers: `Header` gains `admin_href: Option<String>`), `static/style.css`,
`tests/ui_admin.rs`, access matrix rows.

- [ ] Tests: anonymous and non-superuser get 404 on every `/ui/admin*` GET and POST; a
  superuser sees the overview numbers, the users table with search, the rooms table; each
  form posts, redirects back with a flash, and the change is visible; the Admin link shows
  for superusers only.
- [ ] Pages: overview as a few number blocks plus the two short tables; users and rooms
  tables with row actions (`details.edit` for quota/limit/owner inputs, danger buttons with
  `data-confirm` for delete). Screenshot at 1440 and 390 wide before committing.
- [ ] Gate, commit "Add the administration pages".
