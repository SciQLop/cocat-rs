//! The server side of the administrator pages: one read model for the dashboard and one
//! function per action, each reusing the path the ordinary UI uses.
//!
//! Nothing here decides permissions on its own beyond the superuser check: a caller who is
//! not a superuser gets [`AdminError::NotFound`] from every function, so a probe cannot tell
//! the admin surface apart from a missing page.

use std::fs;
use std::sync::OnceLock;
use std::time::Instant;

use cocat_core::role::Role;
use uuid::Uuid;

use crate::AppState;
use crate::accounts::{self, AccountError, Change};
use crate::db::{self, DbError, User};
use crate::lifecycle;
use crate::room::CloseReason;

/// The largest quota an administrator may set, in MB (one terabyte).
const MAX_QUOTA_MB: u64 = 1_048_576;

const MEGABYTE: u64 = 1024 * 1024;

/// Owners whose used storage reaches this share of their quota are reported as near it.
const NEAR_QUOTA_PERCENT: u64 = 80;

static STARTED: OnceLock<Instant> = OnceLock::new();

/// Notes the moment the server started, for the uptime line.
pub fn record_start() {
    let _ = STARTED.set(Instant::now());
}

fn started() -> Instant {
    *STARTED.get_or_init(Instant::now)
}

#[derive(Debug, Clone)]
pub struct Overview {
    pub users: UserCounts,
    pub rooms: RoomCounts,
    pub storage: StorageSummary,
    pub server: ServerInfo,
}

#[derive(Debug, Clone, Copy)]
pub struct UserCounts {
    pub total: u64,
    pub active: u64,
    pub unverified: u64,
    pub superusers: u64,
    pub recent: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct RoomCounts {
    pub total: u64,
    pub open: u64,
    pub clients: u64,
}

#[derive(Debug, Clone)]
pub struct StorageSummary {
    pub total_bytes: u64,
    pub largest: Vec<RoomRow>,
    pub owners_near_quota: Vec<UserRow>,
}

#[derive(Debug, Clone, Copy)]
pub struct ServerInfo {
    pub version: &'static str,
    pub uptime_secs: u64,
    pub resident_bytes: Option<u64>,
    pub room_limit_bytes: u64,
    pub owner_limit_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct UserRow {
    pub id: Uuid,
    pub email: String,
    pub created_at: String,
    pub is_active: bool,
    pub is_verified: bool,
    pub is_superuser: bool,
    pub is_curator: bool,
    pub rooms_owned: u64,
    pub used_bytes: u64,
    pub quota_bytes: u64,
    pub quota_override_mb: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct RoomRow {
    pub id: String,
    pub name: String,
    pub owner: Option<String>,
    pub size_bytes: u64,
    pub limit_bytes: u64,
    pub limit_override_mb: Option<u64>,
    pub members: u64,
    pub public: bool,
    pub open: bool,
    pub clients: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error("not found")]
    NotFound,
    #[error("the account you are signed in with cannot be changed here")]
    OwnAccount,
    #[error("a quota must be between 1 and {MAX_QUOTA_MB} MB")]
    InvalidQuota,
    #[error("no such account")]
    UnknownUser,
    #[error("a public catalogue has no owner")]
    PublicRoom,
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Account(#[from] AccountError),
}

fn require(actor: &User) -> Result<(), AdminError> {
    if actor.is_superuser {
        Ok(())
    } else {
        Err(AdminError::NotFound)
    }
}

fn megabytes(mb: u64) -> u64 {
    mb.saturating_mul(MEGABYTE)
}

fn quota(mb: Option<u64>) -> Result<Option<u64>, AdminError> {
    match mb {
        Some(mb) if mb == 0 || mb > MAX_QUOTA_MB => Err(AdminError::InvalidQuota),
        other => Ok(other),
    }
}

fn near_quota(user: &UserRow) -> bool {
    user.quota_bytes > 0
        && user.used_bytes.saturating_mul(100) >= user.quota_bytes * NEAR_QUOTA_PERCENT
}

pub async fn overview(state: &AppState, actor: &User) -> Result<Overview, AdminError> {
    require(actor)?;
    let (total, active, unverified, superusers, recent) =
        db::admin_user_counts(&state.pool).await?;
    let (open, clients) = state.rooms.open_counts().await;
    let all_rooms = rooms(state, actor).await?;
    let total_bytes = all_rooms.iter().map(|room| room.size_bytes).sum();
    let largest = all_rooms.iter().take(10).cloned().collect();
    let owners_near_quota = users(state, actor, None)
        .await?
        .into_iter()
        .filter(near_quota)
        .collect();
    let limits = state.rooms.limits();
    Ok(Overview {
        users: UserCounts {
            total,
            active,
            unverified,
            superusers,
            recent,
        },
        rooms: RoomCounts {
            total: db::count_rooms(&state.pool).await?,
            open,
            clients,
        },
        storage: StorageSummary {
            total_bytes,
            largest,
            owners_near_quota,
        },
        server: ServerInfo {
            version: env!("CARGO_PKG_VERSION"),
            uptime_secs: started().elapsed().as_secs(),
            resident_bytes: resident_bytes(),
            room_limit_bytes: limits.room,
            owner_limit_bytes: limits.owner,
        },
    })
}

pub async fn users(
    state: &AppState,
    actor: &User,
    search: Option<&str>,
) -> Result<Vec<UserRow>, AdminError> {
    require(actor)?;
    let limits = state.rooms.limits();
    let mut rows = Vec::new();
    for user in db::admin_users(&state.pool, search).await? {
        // simplify: one query and one stat per account; the admin page is not a hot path.
        let mut used_bytes = 0u64;
        for room in db::owned_rooms(&state.pool, user.id).await? {
            used_bytes = used_bytes.saturating_add(state.rooms.room_size(&room));
        }
        rows.push(UserRow {
            id: user.id,
            email: user.email,
            created_at: user.created_at,
            is_active: user.is_active,
            is_verified: user.is_verified,
            is_superuser: user.is_superuser,
            is_curator: user.is_curator,
            rooms_owned: user.rooms_owned,
            used_bytes,
            quota_bytes: user.quota_mb.map(megabytes).unwrap_or(limits.owner),
            quota_override_mb: user.quota_mb,
        });
    }
    Ok(rows)
}

pub async fn rooms(state: &AppState, actor: &User) -> Result<Vec<RoomRow>, AdminError> {
    require(actor)?;
    let limits = state.rooms.limits();
    let mut rows = Vec::new();
    for room in db::admin_rooms(&state.pool).await? {
        rows.push(RoomRow {
            id: room.id.clone(),
            name: room.name,
            owner: room.owner,
            size_bytes: state.rooms.room_size(&room.id),
            limit_bytes: room.limit_mb.map(megabytes).unwrap_or(limits.room),
            limit_override_mb: room.limit_mb,
            members: room.members,
            public: room.public,
            open: state.rooms.is_open(&room.id),
            clients: state.rooms.clients_of(&room.id).await,
        });
    }
    rows.sort_by(|left, right| {
        right
            .size_bytes
            .cmp(&left.size_bytes)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(rows)
}

pub async fn set_user_flags(
    state: &AppState,
    actor: &User,
    user: Uuid,
    active: Option<bool>,
    verified: Option<bool>,
    superuser: Option<bool>,
) -> Result<(), AdminError> {
    require(actor)?;
    let target = db::user_by_id(&state.pool, user)
        .await?
        .ok_or(AdminError::UnknownUser)?;
    if user == actor.id && (active == Some(false) || superuser == Some(false)) {
        return Err(AdminError::OwnAccount);
    }
    accounts::change(
        state,
        target,
        Change {
            email: None,
            password: None,
            is_active: active,
            is_superuser: superuser,
            is_verified: verified,
        },
    )
    .await?;
    Ok(())
}

/// Marks the account a curator of the public catalogues, or no longer.
pub async fn set_curator(
    state: &AppState,
    actor: &User,
    user: Uuid,
    curator: bool,
) -> Result<(), AdminError> {
    require(actor)?;
    if !db::set_curator(&state.pool, user, curator).await? {
        return Err(AdminError::UnknownUser);
    }
    accounts::revoke_public(state, user, CloseReason::AccessChanged).await?;
    Ok(())
}

pub async fn delete_user(state: &AppState, actor: &User, user: Uuid) -> Result<(), AdminError> {
    require(actor)?;
    if user == actor.id {
        return Err(AdminError::OwnAccount);
    }
    if !accounts::delete(state, user).await? {
        return Err(AdminError::UnknownUser);
    }
    Ok(())
}

pub async fn set_user_quota(
    state: &AppState,
    actor: &User,
    user: Uuid,
    mb: Option<u64>,
) -> Result<(), AdminError> {
    require(actor)?;
    let mb = quota(mb)?;
    if !db::set_user_quota(&state.pool, user, mb).await? {
        return Err(AdminError::UnknownUser);
    }
    Ok(())
}

pub async fn set_room_limit(
    state: &AppState,
    actor: &User,
    room: &str,
    mb: Option<u64>,
) -> Result<(), AdminError> {
    require(actor)?;
    let mb = quota(mb)?;
    if !db::set_room_quota(&state.pool, room, mb).await? {
        return Err(AdminError::NotFound);
    }
    Ok(())
}

/// Hands the room to any account, member or not, and also repairs a room whose owner was
/// deleted. The lifecycle transfer would refuse both: it acts as the current owner and only
/// promotes a member. The writes and the socket closing are the same.
pub async fn set_room_owner(
    state: &AppState,
    actor: &User,
    room: &str,
    owner_email: &str,
) -> Result<(), AdminError> {
    require(actor)?;
    let target = db::user_by_email(&state.pool, owner_email)
        .await?
        .ok_or(AdminError::UnknownUser)?;
    if !db::room_exists(&state.pool, room).await? {
        return Err(AdminError::NotFound);
    }
    if db::is_public(&state.pool, room).await? {
        return Err(AdminError::PublicRoom);
    }
    let previous = db::room_owner(&state.pool, room).await?;
    db::add_member(&state.pool, room, target.id, Role::Editor).await?;
    db::set_owner(&state.pool, room, target.id).await?;
    for changed in previous.into_iter().chain([target.id]) {
        state
            .rooms
            .revoke_project(&state.pool, room, changed, CloseReason::AccessChanged)
            .await;
    }
    Ok(())
}

/// Deletes the room whoever owns it, an ownerless room included; the same removal and socket
/// closing as `lifecycle::delete_room`, without its owner check.
pub async fn delete_room(state: &AppState, actor: &User, room: &str) -> Result<(), AdminError> {
    require(actor)?;
    if !lifecycle::remove_project(state, room).await? {
        return Err(AdminError::NotFound);
    }
    Ok(())
}

/// The resident set size of this process, when the kernel exposes it.
fn resident_bytes() -> Option<u64> {
    let rollup = fs::read_to_string("/proc/self/smaps_rollup").ok()?;
    let line = rollup
        .lines()
        .find(|line| line.starts_with("Rss:"))?
        .split_whitespace()
        .nth(1)?;
    let kilobytes: u64 = line.parse().ok()?;
    Some(kilobytes.saturating_mul(1024))
}
