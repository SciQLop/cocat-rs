//! User groups and their members: the rules over the three group tables.
//! A room is shared with a group by a grant, and `db::role_in` folds that grant into
//! the role a member of the group holds on the room.

use sqlx::{Row, Sqlite, Transaction};
use thiserror::Error;
use uuid::Uuid;

use crate::AppState;
use cocat_core::role::Role;

use crate::db::{self, DbError};
use crate::lifecycle::{self, LifecycleError};

pub const OWNER_CANNOT_LEAVE: &str = "The owner cannot leave the group";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupRow {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupRole {
    Owner,
    Member,
}

impl GroupRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Member => "member",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMember {
    pub user_id: Uuid,
    pub email: String,
    pub role: GroupRole,
}

pub async fn create(state: &AppState, actor: Uuid, name: &str) -> Result<GroupRow, LifecycleError> {
    let id = Uuid::new_v4().simple().to_string();
    let mut tx = state.pool.begin().await?;
    sqlx::query("INSERT INTO user_group (id, name, created_by) VALUES (?, ?, ?)")
        .bind(&id)
        .bind(name)
        .bind(actor.to_string())
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO group_member (group_id, user_id, role) VALUES (?, ?, 'owner')")
        .bind(&id)
        .bind(actor.to_string())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(GroupRow {
        id,
        name: name.to_owned(),
    })
}

pub async fn list_for(
    state: &AppState,
    user: Uuid,
) -> Result<Vec<(GroupRow, GroupRole)>, LifecycleError> {
    let rows = sqlx::query(
        "SELECT user_group.id, user_group.name, group_member.role
         FROM group_member JOIN user_group ON user_group.id = group_member.group_id
         WHERE group_member.user_id = ?
         ORDER BY user_group.name, user_group.id",
    )
    .bind(user.to_string())
    .fetch_all(&state.pool)
    .await?;
    let mut groups = Vec::new();
    for row in &rows {
        groups.push((
            GroupRow {
                id: row.try_get("id")?,
                name: row.try_get("name")?,
            },
            role_from_name(&row.try_get::<String, _>("role")?)?,
        ));
    }
    Ok(groups)
}

pub async fn rename(
    state: &AppState,
    group_id: &str,
    actor: Uuid,
    name: &str,
) -> Result<GroupRow, LifecycleError> {
    require_owner(state, group_id, actor).await?;
    let updated = sqlx::query("UPDATE user_group SET name = ? WHERE id = ?")
        .bind(name)
        .bind(group_id)
        .execute(&state.pool)
        .await?
        .rows_affected()
        > 0;
    if !updated {
        return Err(not_found(group_id));
    }
    Ok(GroupRow {
        id: group_id.to_owned(),
        name: name.to_owned(),
    })
}

pub async fn delete(state: &AppState, group_id: &str, actor: Uuid) -> Result<(), LifecycleError> {
    require_owner(state, group_id, actor).await?;
    let mut tx = db::begin_immediate(&state.pool).await?;
    let pairs = member_rooms(&mut tx, group_id).await?;
    let deleted = sqlx::query("DELETE FROM user_group WHERE id = ?")
        .bind(group_id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
        > 0;
    if !deleted {
        return Err(not_found(group_id));
    }
    tx.commit().await?;
    revoke_pairs(state, pairs).await;
    Ok(())
}

pub async fn members(
    state: &AppState,
    group_id: &str,
    actor: Uuid,
) -> Result<Vec<GroupMember>, LifecycleError> {
    require_member(state, group_id, actor).await?;
    let rows = sqlx::query(
        r#"SELECT "user".id, "user".email, group_member.role
           FROM group_member JOIN "user" ON "user".id = group_member.user_id
           WHERE group_member.group_id = ?
           ORDER BY "user".email"#,
    )
    .bind(group_id)
    .fetch_all(&state.pool)
    .await?;
    let mut members = Vec::new();
    for row in &rows {
        members.push(GroupMember {
            user_id: parse_uuid(&row.try_get::<String, _>("id")?)?,
            email: row.try_get("email")?,
            role: role_from_name(&row.try_get::<String, _>("role")?)?,
        });
    }
    Ok(members)
}

/// The owner may remove anyone but themselves; every member may leave.
pub async fn remove_member(
    state: &AppState,
    group_id: &str,
    actor: Uuid,
    member: Uuid,
) -> Result<(), LifecycleError> {
    let actor_role = require_member(state, group_id, actor).await?;
    if actor_role != GroupRole::Owner && actor != member {
        return Err(forbidden(group_id));
    }
    if group_role(state, group_id, member).await? == Some(GroupRole::Owner) {
        return Err(LifecycleError::Conflict(OWNER_CANNOT_LEAVE));
    }
    let mut tx = db::begin_immediate(&state.pool).await?;
    let rooms = granted_rooms(&mut tx, group_id).await?;
    let removed = sqlx::query("DELETE FROM group_member WHERE group_id = ? AND user_id = ?")
        .bind(group_id)
        .bind(member.to_string())
        .execute(&mut *tx)
        .await?
        .rows_affected()
        > 0;
    if !removed {
        return Err(not_a_member(group_id, member));
    }
    tx.commit().await?;
    let pairs = rooms.into_iter().map(|room_id| (room_id, member)).collect();
    revoke_pairs(state, pairs).await;
    Ok(())
}

/// Shares a room with a group. The actor must own the room and belong to the group, so a
/// group they cannot see answers 404. A changed grant revokes the group's members in that
/// room, because their sockets carry the old role.
pub async fn grant(
    state: &AppState,
    room_id: &str,
    group_id: &str,
    actor: Uuid,
    role: Role,
) -> Result<(GroupRow, Role), LifecycleError> {
    lifecycle::require_role(state, room_id, actor, Role::Owner).await?;
    if role == Role::Owner {
        return Err(LifecycleError::Invalid(
            "A group cannot be granted the owner role",
        ));
    }
    let group = group_row(state, group_id).await?;
    if group_role(state, group_id, actor).await?.is_none() {
        return Err(not_found(group_id));
    }
    let mut tx = db::begin_immediate(&state.pool).await?;
    let previous: Option<String> =
        sqlx::query_scalar("SELECT role FROM room_group WHERE room_id = ? AND group_id = ?")
            .bind(room_id)
            .bind(group_id)
            .fetch_optional(&mut *tx)
            .await?;
    let changed = previous
        .as_deref()
        .is_some_and(|previous| previous != role.as_str());
    let members = match changed {
        true => members_of(&mut tx, group_id).await?,
        false => Vec::new(),
    };
    sqlx::query(
        "INSERT INTO room_group (room_id, group_id, role) VALUES (?, ?, ?)
         ON CONFLICT (room_id, group_id) DO UPDATE SET role = excluded.role",
    )
    .bind(room_id)
    .bind(group_id)
    .bind(role.as_str())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    revoke_pairs(state, in_room(room_id, members)).await;
    Ok((group, role))
}

/// Takes the room away from the group. The actor must own the room; they need not belong
/// to the group any more.
pub async fn ungrant(
    state: &AppState,
    room_id: &str,
    group_id: &str,
    actor: Uuid,
) -> Result<(), LifecycleError> {
    lifecycle::require_role(state, room_id, actor, Role::Owner).await?;
    let mut tx = db::begin_immediate(&state.pool).await?;
    let members = members_of(&mut tx, group_id).await?;
    let deleted = sqlx::query("DELETE FROM room_group WHERE room_id = ? AND group_id = ?")
        .bind(room_id)
        .bind(group_id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
        > 0;
    if !deleted {
        return Err(LifecycleError::NotFound(format!(
            "Group {group_id} is not granted room {room_id}"
        )));
    }
    tx.commit().await?;
    revoke_pairs(state, in_room(room_id, members)).await;
    Ok(())
}

/// The groups a room is shared with, for any member of the room.
pub async fn room_groups(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
) -> Result<Vec<(GroupRow, Role)>, LifecycleError> {
    lifecycle::require_role(state, room_id, actor, Role::Viewer).await?;
    let rows = sqlx::query(
        "SELECT user_group.id, user_group.name, room_group.role
         FROM room_group JOIN user_group ON user_group.id = room_group.group_id
         WHERE room_group.room_id = ?
         ORDER BY user_group.name, user_group.id",
    )
    .bind(room_id)
    .fetch_all(&state.pool)
    .await?;
    let mut groups = Vec::new();
    for row in &rows {
        groups.push((
            GroupRow {
                id: row.try_get("id")?,
                name: row.try_get("name")?,
            },
            row.try_get::<String, _>("role")?.parse::<Role>()?,
        ));
    }
    Ok(groups)
}

async fn group_row(state: &AppState, group_id: &str) -> Result<GroupRow, LifecycleError> {
    let row = sqlx::query("SELECT id, name FROM user_group WHERE id = ?")
        .bind(group_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| not_found(group_id))?;
    Ok(GroupRow {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
    })
}

async fn members_of(
    tx: &mut Transaction<'_, Sqlite>,
    group_id: &str,
) -> Result<Vec<Uuid>, LifecycleError> {
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT user_id FROM group_member WHERE group_id = ?")
            .bind(group_id)
            .fetch_all(&mut **tx)
            .await?;
    let mut members = Vec::new();
    for raw in &rows {
        members.push(parse_uuid(raw)?);
    }
    Ok(members)
}

fn in_room(room_id: &str, members: Vec<Uuid>) -> Vec<(String, Uuid)> {
    members
        .into_iter()
        .map(|member| (room_id.to_owned(), member))
        .collect()
}

async fn group_role(
    state: &AppState,
    group_id: &str,
    user: Uuid,
) -> Result<Option<GroupRole>, LifecycleError> {
    let name: Option<String> =
        sqlx::query_scalar("SELECT role FROM group_member WHERE group_id = ? AND user_id = ?")
            .bind(group_id)
            .bind(user.to_string())
            .fetch_optional(&state.pool)
            .await?;
    Ok(name.as_deref().map(role_from_name).transpose()?)
}

async fn require_member(
    state: &AppState,
    group_id: &str,
    actor: Uuid,
) -> Result<GroupRole, LifecycleError> {
    group_role(state, group_id, actor)
        .await?
        .ok_or_else(|| not_found(group_id))
}

async fn require_owner(
    state: &AppState,
    group_id: &str,
    actor: Uuid,
) -> Result<(), LifecycleError> {
    match require_member(state, group_id, actor).await? {
        GroupRole::Owner => Ok(()),
        GroupRole::Member => Err(forbidden(group_id)),
    }
}

/// Every (member, room) pair whose access the group carried, read before the delete
/// because the cascade takes both tables with it.
async fn member_rooms(
    tx: &mut Transaction<'_, Sqlite>,
    group_id: &str,
) -> Result<Vec<(String, Uuid)>, LifecycleError> {
    let rows = sqlx::query(
        "SELECT room_group.room_id, group_member.user_id
         FROM group_member JOIN room_group ON room_group.group_id = group_member.group_id
         WHERE group_member.group_id = ?",
    )
    .bind(group_id)
    .fetch_all(&mut **tx)
    .await?;
    let mut pairs = Vec::new();
    for row in &rows {
        pairs.push((
            row.try_get("room_id")?,
            parse_uuid(&row.try_get::<String, _>("user_id")?)?,
        ));
    }
    Ok(pairs)
}

async fn granted_rooms(
    tx: &mut Transaction<'_, Sqlite>,
    group_id: &str,
) -> Result<Vec<String>, LifecycleError> {
    let rooms = sqlx::query_scalar("SELECT room_id FROM room_group WHERE group_id = ?")
        .bind(group_id)
        .fetch_all(&mut **tx)
        .await?;
    Ok(rooms)
}

async fn revoke_pairs(state: &AppState, pairs: Vec<(String, Uuid)>) {
    for (room_id, user) in pairs {
        state.rooms.revoke(&room_id, user).await;
    }
}

fn role_from_name(name: &str) -> Result<GroupRole, DbError> {
    match name {
        "owner" => Ok(GroupRole::Owner),
        "member" => Ok(GroupRole::Member),
        other => Err(DbError::Sqlx(sqlx::Error::Decode(Box::new(
            UnknownGroupRole(other.to_owned()),
        )))),
    }
}

fn parse_uuid(raw: &str) -> Result<Uuid, DbError> {
    Uuid::parse_str(raw).map_err(|error| DbError::Sqlx(sqlx::Error::Decode(Box::new(error))))
}

fn not_found(group_id: &str) -> LifecycleError {
    LifecycleError::NotFound(format!("Group {group_id} not found"))
}

fn forbidden(group_id: &str) -> LifecycleError {
    LifecycleError::Forbidden(format!("Group {group_id} requires the owner role"))
}

fn not_a_member(group_id: &str, member: Uuid) -> LifecycleError {
    LifecycleError::NotFound(format!("User {member} is not a member of group {group_id}"))
}

#[derive(Debug, Error)]
#[error("unknown group role: {0:?}")]
struct UnknownGroupRole(String);
