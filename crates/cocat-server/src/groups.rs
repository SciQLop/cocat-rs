//! User groups and their members: the rules over the three group tables.
//! A room is shared with a group by a grant, and `db::role_in` folds that grant into
//! the role a member of the group holds on the room.

use sqlx::sqlite::SqliteRow;
use sqlx::{AssertSqlSafe, Row, Sqlite, Transaction};
use thiserror::Error;
use uuid::Uuid;

use crate::AppState;
use crate::room::CloseReason;
use cocat_core::role::Role;

use crate::db::{self, DbError};
use crate::invitations;
use crate::lifecycle::{self, LifecycleError};
use crate::mail::{self, Mailer};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupInvitation {
    pub id: i64,
    pub email: Option<String>,
    pub expires_at: String,
    pub uses: i64,
    pub max_uses: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupPreview {
    pub group_id: String,
    pub group_name: String,
    pub inviter_email: Option<String>,
    pub expires_at: String,
}

pub async fn create(state: &AppState, actor: Uuid, name: &str) -> Result<GroupRow, LifecycleError> {
    let id = Uuid::new_v4().simple().to_string();
    let name = lifecycle::clean_name(name, lifecycle::Noun::Group)?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("INSERT INTO user_group (id, name, created_by) VALUES (?, ?, ?)")
        .bind(&id)
        .bind(&name)
        .bind(actor.to_string())
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO group_member (group_id, user_id, role) VALUES (?, ?, 'owner')")
        .bind(&id)
        .bind(actor.to_string())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(GroupRow { id, name })
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
    let name = lifecycle::clean_name(name, lifecycle::Noun::Group)?;
    let updated = sqlx::query("UPDATE user_group SET name = ? WHERE id = ?")
        .bind(&name)
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
        name,
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

/// Shares a room with a group. The actor must be allowed to share the room and belong to the
/// group, so a group they cannot see answers 404. A changed grant revokes the group's members in that
/// room, because their sockets carry the old role.
pub async fn grant(
    state: &AppState,
    room_id: &str,
    group_id: &str,
    actor: Uuid,
    role: Role,
) -> Result<(GroupRow, Role), LifecycleError> {
    lifecycle::require_sharer(state, room_id, actor).await?;
    lifecycle::refuse_personal(state, room_id).await?;
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

/// Takes the room away from the group. The actor must be allowed to share the room; they
/// need not belong to the group any more.
pub async fn ungrant(
    state: &AppState,
    room_id: &str,
    group_id: &str,
    actor: Uuid,
) -> Result<(), LifecycleError> {
    lifecycle::require_sharer(state, room_id, actor).await?;
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
            "Group {group_id} is not granted project {room_id}"
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
    lifecycle::refuse_public(state, room_id).await?;
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

/// The rooms shared with a group, for any member of the group.
pub async fn shared_rooms(
    state: &AppState,
    group_id: &str,
    actor: Uuid,
) -> Result<Vec<(db::RoomRow, Role)>, LifecycleError> {
    require_member(state, group_id, actor).await?;
    let rows = sqlx::query(
        "SELECT room.id, room.name, room_group.role
         FROM room_group JOIN room ON room.id = room_group.room_id
         WHERE room_group.group_id = ?
         ORDER BY room.name, room.id",
    )
    .bind(group_id)
    .fetch_all(&state.pool)
    .await?;
    let mut rooms = Vec::new();
    for row in &rows {
        rooms.push((
            db::RoomRow {
                id: row.try_get("id")?,
                name: row.try_get("name")?,
            },
            row.try_get::<String, _>("role")?.parse::<Role>()?,
        ));
    }
    Ok(rooms)
}

/// A link that admits its bearer to the group as a member. The token comes back empty when it
/// was mailed, so a reply cannot carry a second copy of a link that travelled to its addressee.
pub async fn create_invitation(
    state: &AppState,
    group_id: &str,
    actor: Uuid,
    email: Option<&str>,
    expires_in_days: u32,
    max_uses: Option<u32>,
) -> Result<(i64, String), LifecycleError> {
    require_owner(state, group_id, actor).await?;
    let (token, hash) = invitations::new_token()?;
    let max_uses = if email.is_some() { Some(1) } else { max_uses };
    let id = sqlx::query(
        "INSERT INTO group_invitation
             (token_hash, group_id, email, created_by, expires_at, max_uses)
         VALUES (?, ?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?), ?)",
    )
    .bind(hash)
    .bind(group_id)
    .bind(email)
    .bind(actor.to_string())
    .bind(format!("+{expires_in_days} days"))
    .bind(max_uses.map(i64::from))
    .execute(&state.pool)
    .await?
    .last_insert_rowid();
    if let (Some(mailer), Some(address)) = (&state.mailer, email) {
        mail_invitation(state, mailer, actor, group_id, id, &token, address).await?;
        return Ok((id, String::new()));
    }
    Ok((id, token))
}

pub async fn list_invitations(
    state: &AppState,
    group_id: &str,
    actor: Uuid,
) -> Result<Vec<GroupInvitation>, LifecycleError> {
    require_owner(state, group_id, actor).await?;
    let predicate = invitations::usable("group_invitation");
    let rows = sqlx::query(AssertSqlSafe(format!(
        "SELECT id, email, expires_at, uses, max_uses
         FROM group_invitation
         WHERE group_id = ? AND {predicate}
         ORDER BY created_at DESC, id DESC"
    )))
    .bind(group_id)
    .fetch_all(&state.pool)
    .await?;
    rows.iter().map(invitation_from_row).collect()
}

pub async fn revoke_invitation(
    state: &AppState,
    group_id: &str,
    actor: Uuid,
    invitation_id: i64,
) -> Result<(), LifecycleError> {
    require_owner(state, group_id, actor).await?;
    let revoked =
        sqlx::query("UPDATE group_invitation SET revoked = TRUE WHERE id = ? AND group_id = ?")
            .bind(invitation_id)
            .bind(group_id)
            .execute(&state.pool)
            .await?;
    if revoked.rows_affected() != 1 {
        return Err(invitation_not_found(invitation_id));
    }
    Ok(())
}

/// No cookie: the guest has to see what the link offers before signing in.
pub async fn preview_invitation(
    state: &AppState,
    token: &str,
) -> Result<GroupPreview, LifecycleError> {
    let predicate = invitations::usable("group_invitation");
    let row = sqlx::query(AssertSqlSafe(format!(
        "SELECT group_invitation.group_id, group_invitation.expires_at, user_group.name,
                \"user\".email AS inviter_email
         FROM group_invitation
         JOIN user_group ON user_group.id = group_invitation.group_id
         LEFT JOIN \"user\" ON \"user\".id = group_invitation.created_by
         WHERE group_invitation.token_hash = ? AND {predicate}"
    )))
    .bind(invitations::hash(token))
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| invitation_not_found(0))?;
    Ok(GroupPreview {
        group_id: row.try_get("group_id")?,
        group_name: row.try_get("name")?,
        inviter_email: row.try_get("inviter_email")?,
        expires_at: row.try_get("expires_at")?,
    })
}

/// Joins `user` as a member, spending one use. An existing member is answered without spending
/// anything. The use and the membership are written in one immediate transaction, and the
/// rooms the group carries are read in it too, so nothing can slip in between and stay
/// unrevoked.
pub async fn accept_invitation(
    state: &AppState,
    token: &str,
    user: &db::User,
) -> Result<GroupRow, LifecycleError> {
    let predicate = invitations::usable("group_invitation");
    let row = sqlx::query(AssertSqlSafe(format!(
        "SELECT group_invitation.id, group_invitation.group_id, group_invitation.email,
                user_group.name
         FROM group_invitation JOIN user_group ON user_group.id = group_invitation.group_id
         WHERE group_invitation.token_hash = ? AND {predicate}"
    )))
    .bind(invitations::hash(token))
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| invitation_not_found(0))?;
    let id: i64 = row.try_get("id")?;
    let group = GroupRow {
        id: row.try_get("group_id")?,
        name: row.try_get("name")?,
    };
    let addressed: Option<String> = row.try_get("email")?;
    if let Some(addressed) = addressed
        && !addressed.eq_ignore_ascii_case(&user.email)
    {
        return Err(LifecycleError::Forbidden(
            "This invitation is for another email address".to_owned(),
        ));
    }
    let mut tx = db::begin_immediate(&state.pool).await?;
    let member: Option<String> =
        sqlx::query_scalar("SELECT role FROM group_member WHERE group_id = ? AND user_id = ?")
            .bind(&group.id)
            .bind(user.id.to_string())
            .fetch_optional(&mut *tx)
            .await?;
    if member.is_some() {
        return Ok(group);
    }
    let rooms: Vec<String> =
        sqlx::query_scalar("SELECT room_id FROM room_group WHERE group_id = ?")
            .bind(&group.id)
            .fetch_all(&mut *tx)
            .await?;
    let spent = sqlx::query(AssertSqlSafe(format!(
        "UPDATE group_invitation SET uses = uses + 1 WHERE group_invitation.id = ? AND {predicate}"
    )))
    .bind(id)
    .execute(&mut *tx)
    .await?;
    if spent.rows_affected() != 1 {
        return Err(invitation_not_found(id));
    }
    sqlx::query("INSERT INTO group_member (group_id, user_id, role) VALUES (?, ?, 'member')")
        .bind(&group.id)
        .bind(user.id.to_string())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let pairs = rooms
        .into_iter()
        .map(|room_id| (room_id, user.id))
        .collect();
    revoke_pairs(state, pairs).await;
    Ok(group)
}

/// Mail the link to the address the invitation names, and undo the invitation if the relay
/// refuses, as a room invitation does. The token never reaches the log.
async fn mail_invitation(
    state: &AppState,
    mailer: &Mailer,
    actor: Uuid,
    group_id: &str,
    id: i64,
    token: &str,
    address: &str,
) -> Result<(), LifecycleError> {
    let row = sqlx::query(
        "SELECT user_group.name, \"user\".email
         FROM user_group JOIN \"user\" ON \"user\".id = ?
         WHERE user_group.id = ?",
    )
    .bind(actor.to_string())
    .bind(group_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| not_found(group_id))?;
    let group_name: String = row.try_get("name")?;
    let inviter: String = row.try_get("email")?;
    let (subject, body) =
        mail::group_invitation_mail(&group_name, &inviter, &state.links.group_invitation(token));
    if let Err(error) = mailer.send(address, &subject, &body).await {
        tracing::warn!(%error, "could not mail the group invitation");
        sqlx::query("DELETE FROM group_invitation WHERE id = ?")
            .bind(id)
            .execute(&state.pool)
            .await?;
        return Err(LifecycleError::Invalid(
            "The email could not be sent; no invitation was created",
        ));
    }
    Ok(())
}

fn invitation_from_row(row: &SqliteRow) -> Result<GroupInvitation, LifecycleError> {
    Ok(GroupInvitation {
        id: row.try_get("id")?,
        email: row.try_get("email")?,
        expires_at: row.try_get("expires_at")?,
        uses: row.try_get("uses")?,
        max_uses: row.try_get("max_uses")?,
    })
}

fn invitation_not_found(invitation_id: i64) -> LifecycleError {
    match invitation_id {
        0 => LifecycleError::NotFound("Invitation not found".to_owned()),
        id => LifecycleError::NotFound(format!("Invitation {id} not found")),
    }
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

pub(crate) async fn require_member(
    state: &AppState,
    group_id: &str,
    actor: Uuid,
) -> Result<GroupRole, LifecycleError> {
    group_role(state, group_id, actor)
        .await?
        .ok_or_else(|| not_found(group_id))
}

pub(crate) async fn require_owner(
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
        state
            .rooms
            .revoke(&room_id, user, CloseReason::AccessChanged)
            .await;
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
