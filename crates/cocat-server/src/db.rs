//! The server's SQLite schema, applied by sqlx migrations, and the queries on it.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::path::Path;
use std::str::FromStr;

use argon2::Argon2;
use argon2::password_hash::PasswordHasher;
use sqlx::sqlite::{SqliteConnectOptions, SqliteRow};
use sqlx::{Executor, Row, Sqlite, SqlitePool, Transaction};
use thiserror::Error;
use uuid::Uuid;

use cocat_core::role::Role;
use cocat_core::room_id::{InvalidRoomId, validate_room_id};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Migrations(#[from] sqlx::migrate::MigrateError),
    #[error("hashing a password: {0}")]
    Hash(#[from] argon2::password_hash::Error),
    #[error("the password task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
    #[error(transparent)]
    InvalidRoomId(#[from] InvalidRoomId),
    #[error("{user_id} is not a member of room {room_id}")]
    NoSuchMembership { room_id: String, user_id: Uuid },
    #[error("{email} already has an account")]
    EmailTaken { email: String },
    #[error(
        "these projects have ids starting with \"{}\", which this server keeps for public \
         catalogues: {}. They came from a Python server: rename them there and import again",
        cocat_core::room_id::PUBLIC_PREFIX,
        .0.join(", ")
    )]
    ReservedIds(Vec<String>),
    #[error(
        "\"{0}\" starts with \"{prefix}\", which this server keeps for public catalogues: \
         choose another id",
        prefix = cocat_core::room_id::PUBLIC_PREFIX
    )]
    ReservedId(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub hashed_password: String,
    pub is_active: bool,
    pub is_superuser: bool,
    pub is_verified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomRow {
    pub id: String,
    pub name: String,
}

pub async fn open(db_path: &Path) -> Result<SqlitePool, DbError> {
    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePool::connect_with(options).await?;
    MIGRATOR.run(&pool).await?;
    Ok(pool)
}

/// Starts a transaction that takes the write lock before its first read, so no other
/// writer can commit between the reads and the writes that depend on them. Dropping it
/// rolls back.
pub async fn begin_immediate(pool: &SqlitePool) -> Result<Transaction<'static, Sqlite>, DbError> {
    Ok(pool.begin_with("BEGIN IMMEDIATE").await?)
}

pub async fn user_by_email<'e, E: Executor<'e, Database = Sqlite>>(
    executor: E,
    email: &str,
) -> Result<Option<User>, DbError> {
    let row = sqlx::query(
        r#"SELECT id, email, hashed_password, is_active, is_superuser, is_verified
           FROM "user" WHERE email = ?"#,
    )
    .bind(email)
    .fetch_optional(executor)
    .await?;
    Ok(row.as_ref().map(user_from_row).transpose()?)
}

pub async fn user_by_id(pool: &SqlitePool, id: Uuid) -> Result<Option<User>, DbError> {
    let row = sqlx::query(
        r#"SELECT id, email, hashed_password, is_active, is_superuser, is_verified
           FROM "user" WHERE id = ?"#,
    )
    .bind(id.to_string())
    .fetch_optional(pool)
    .await?;
    Ok(row.as_ref().map(user_from_row).transpose()?)
}

/// The rooms `user_id` belongs to, by name then id, with the role they hold there.
pub async fn list_user_rooms(
    executor: impl Executor<'_, Database = Sqlite>,
    user_id: Uuid,
) -> Result<Vec<(RoomRow, Role)>, DbError> {
    let rows = sqlx::query(
        "SELECT room.id, room.name, membership.role
         FROM membership JOIN room ON room.id = membership.room_id
         WHERE membership.user_id = ?
         UNION ALL
         SELECT room.id, room.name, room_group.role
         FROM room_group
           JOIN group_member ON group_member.group_id = room_group.group_id
           JOIN room ON room.id = room_group.room_id
         WHERE group_member.user_id = ?",
    )
    .bind(user_id.to_string())
    .bind(user_id.to_string())
    .fetch_all(executor)
    .await?;
    let mut best: BTreeMap<(String, String), Role> = BTreeMap::new();
    for row in &rows {
        let name: String = row.try_get("name")?;
        let id: String = row.try_get("id")?;
        let role = role_from_name(&row.try_get::<String, _>("role")?)?;
        best.entry((name, id))
            .and_modify(|current| {
                if role.rank() > current.rank() {
                    *current = role;
                }
            })
            .or_insert(role);
    }
    Ok(best
        .into_iter()
        .map(|((name, id), role)| (RoomRow { id, name }, role))
        .collect())
}

/// The members of a room and their roles, by email.
pub async fn list_members(pool: &SqlitePool, room_id: &str) -> Result<Vec<(User, Role)>, DbError> {
    let rows = sqlx::query(
        r#"SELECT "user".id, "user".email, "user".hashed_password, "user".is_active,
                  "user".is_superuser, "user".is_verified, membership.role
           FROM membership JOIN "user" ON "user".id = membership.user_id
           WHERE membership.room_id = ?
           ORDER BY "user".email"#,
    )
    .bind(room_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            let role: String = row.try_get("role")?;
            Ok((user_from_row(row)?, role_from_name(&role)?))
        })
        .collect()
}

/// The emails of the active people `user_id` shares a room or a group with, themselves
/// excluded: what an invitation form may suggest without revealing every account.
pub async fn known_people(pool: &SqlitePool, user_id: Uuid) -> Result<Vec<String>, DbError> {
    let emails = sqlx::query_scalar(
        r#"SELECT email FROM "user"
           WHERE is_active AND id != ?1 AND (
               id IN (SELECT other.user_id FROM membership mine
                      JOIN membership other ON other.room_id = mine.room_id
                      WHERE mine.user_id = ?1)
               OR id IN (SELECT other.user_id FROM group_member mine
                         JOIN group_member other ON other.group_id = mine.group_id
                         WHERE mine.user_id = ?1))
           ORDER BY email"#,
    )
    .bind(user_id.to_string())
    .fetch_all(pool)
    .await?;
    Ok(emails)
}

/// The email of a user reached through their own membership of a room. A user who is not a
/// member has no email here, whether or not an account carries that id.
pub async fn member_email(
    pool: &SqlitePool,
    room_id: &str,
    user_id: Uuid,
) -> Result<Option<String>, DbError> {
    Ok(sqlx::query_scalar(
        "SELECT \"user\".email FROM membership
           JOIN \"user\" ON \"user\".id = membership.user_id
         WHERE membership.room_id = ? AND membership.user_id = ?",
    )
    .bind(room_id)
    .bind(user_id.to_string())
    .fetch_optional(pool)
    .await?)
}

/// The email of a user reached through their own membership of a group.
pub async fn group_member_email(
    pool: &SqlitePool,
    group_id: &str,
    user_id: Uuid,
) -> Result<Option<String>, DbError> {
    Ok(sqlx::query_scalar(
        "SELECT \"user\".email FROM group_member
           JOIN \"user\" ON \"user\".id = group_member.user_id
         WHERE group_member.group_id = ? AND group_member.user_id = ?",
    )
    .bind(group_id)
    .bind(user_id.to_string())
    .fetch_optional(pool)
    .await?)
}

/// The role a user holds on a room through their own membership alone.
pub async fn direct_role(
    pool: &SqlitePool,
    room_id: &str,
    user_id: Uuid,
) -> Result<Option<Role>, DbError> {
    let role: Option<String> =
        sqlx::query("SELECT role FROM membership WHERE room_id = ? AND user_id = ?")
            .bind(room_id)
            .bind(user_id.to_string())
            .fetch_optional(pool)
            .await?
            .map(|row| row.try_get("role"))
            .transpose()?;
    Ok(role.as_deref().map(role_from_name).transpose()?)
}

/// The role a user holds on a room: the highest of their own membership and every
/// grant made to a group they belong to. A grant can never be `owner`, so an owner
/// only ever comes from a direct membership. A public catalogue has neither: every account
/// views it and curators edit it.
pub async fn role_in(
    pool: &SqlitePool,
    room_id: &str,
    user_id: Uuid,
) -> Result<Option<Role>, DbError> {
    let names: Vec<String> = sqlx::query_scalar(
        r#"SELECT role FROM membership WHERE room_id = ?1 AND user_id = ?2
         UNION ALL
         SELECT room_group.role
         FROM room_group
           JOIN group_member ON group_member.group_id = room_group.group_id
         WHERE room_group.room_id = ?1 AND group_member.user_id = ?2
         UNION ALL
         SELECT CASE WHEN "user".is_superuser OR "user".is_curator THEN 'editor' ELSE 'viewer' END
         FROM room JOIN "user" ON "user".id = ?2
         WHERE room.id = ?1 AND room.public"#,
    )
    .bind(room_id)
    .bind(user_id.to_string())
    .fetch_all(pool)
    .await?;
    let roles: Vec<Role> = names
        .iter()
        .map(|name| role_from_name(name))
        .collect::<Result<_, _>>()?;
    Ok(roles.into_iter().max_by_key(Role::rank))
}

/// The room and the role `user_id` holds there, when they reach it at all.
pub async fn room_for(
    pool: &SqlitePool,
    room_id: &str,
    user_id: Uuid,
) -> Result<Option<(RoomRow, Role)>, DbError> {
    let Some(role) = role_in(pool, room_id, user_id).await? else {
        return Ok(None);
    };
    let name = room_name(pool, room_id).await?;
    Ok(name.map(|name| {
        let id = room_id.to_owned();
        (RoomRow { id, name }, role)
    }))
}

pub async fn room_name(pool: &SqlitePool, room_id: &str) -> Result<Option<String>, DbError> {
    Ok(sqlx::query_scalar("SELECT name FROM room WHERE id = ?")
        .bind(room_id)
        .fetch_optional(pool)
        .await?)
}

/// The account that owns a room, if a membership says anyone does.
pub async fn room_owner(pool: &SqlitePool, room_id: &str) -> Result<Option<Uuid>, DbError> {
    let owner: Option<String> =
        sqlx::query_scalar("SELECT user_id FROM membership WHERE room_id = ? AND role = 'owner'")
            .bind(room_id)
            .fetch_optional(pool)
            .await?;
    Ok(owner.and_then(|id| Uuid::parse_str(&id).ok()))
}

/// The rooms `user` owns: what a storage quota counts before the room being opened.
pub async fn owned_rooms(pool: &SqlitePool, user: Uuid) -> Result<Vec<String>, DbError> {
    Ok(
        sqlx::query_scalar("SELECT room_id FROM membership WHERE user_id = ? AND role = 'owner'")
            .bind(user.to_string())
            .fetch_all(pool)
            .await?,
    )
}

/// Whether the account looks after the public catalogues: an administrator, or an account an
/// administrator marked.
pub async fn is_curator(pool: &SqlitePool, user: Uuid) -> Result<bool, DbError> {
    let curator: Option<bool> =
        sqlx::query_scalar(r#"SELECT is_superuser OR is_curator FROM "user" WHERE id = ?"#)
            .bind(user.to_string())
            .fetch_optional(pool)
            .await?;
    Ok(curator.unwrap_or(false))
}

/// Marks the account a curator, or no longer; false for an unknown account.
pub async fn set_curator(pool: &SqlitePool, user: Uuid, curator: bool) -> Result<bool, DbError> {
    let updated = sqlx::query(r#"UPDATE "user" SET is_curator = ? WHERE id = ?"#)
        .bind(curator)
        .bind(user.to_string())
        .execute(pool)
        .await?;
    Ok(updated.rows_affected() > 0)
}

/// The storage quota an account carries, in MB, when an administrator set one.
pub async fn user_quota(pool: &SqlitePool, user: Uuid) -> Result<Option<u64>, DbError> {
    let quota: Option<i64> = sqlx::query_scalar("SELECT quota_mb FROM \"user\" WHERE id = ?")
        .bind(user.to_string())
        .fetch_optional(pool)
        .await?
        .flatten();
    Ok(quota.and_then(|mb| u64::try_from(mb).ok()))
}

pub async fn set_user_quota(
    pool: &SqlitePool,
    user: Uuid,
    quota_mb: Option<u64>,
) -> Result<bool, DbError> {
    let updated = sqlx::query("UPDATE \"user\" SET quota_mb = ? WHERE id = ?")
        .bind(quota_mb.and_then(|mb| i64::try_from(mb).ok()))
        .bind(user.to_string())
        .execute(pool)
        .await?;
    Ok(updated.rows_affected() > 0)
}

/// The storage limit a room carries, in MB, when an administrator set one.
pub async fn room_exists(pool: &SqlitePool, room_id: &str) -> Result<bool, DbError> {
    let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM room WHERE id = ?")
        .bind(room_id)
        .fetch_optional(pool)
        .await?;
    Ok(found.is_some())
}

pub async fn room_quota(pool: &SqlitePool, room_id: &str) -> Result<Option<u64>, DbError> {
    let quota: Option<i64> = sqlx::query_scalar("SELECT limit_mb FROM room WHERE id = ?")
        .bind(room_id)
        .fetch_optional(pool)
        .await?
        .flatten();
    Ok(quota.and_then(|mb| u64::try_from(mb).ok()))
}

pub async fn set_room_quota(
    pool: &SqlitePool,
    room_id: &str,
    limit_mb: Option<u64>,
) -> Result<bool, DbError> {
    let updated = sqlx::query("UPDATE room SET limit_mb = ? WHERE id = ?")
        .bind(limit_mb.and_then(|mb| i64::try_from(mb).ok()))
        .bind(room_id)
        .execute(pool)
        .await?;
    Ok(updated.rows_affected() > 0)
}

/// One account as the admin pages read it: the profile, the number of rooms it owns, and the
/// quota an administrator set (NULL when the configured default applies).
pub struct AdminUser {
    pub id: Uuid,
    pub email: String,
    pub created_at: String,
    pub is_active: bool,
    pub is_verified: bool,
    pub is_superuser: bool,
    pub is_curator: bool,
    pub rooms_owned: u64,
    pub quota_mb: Option<u64>,
}

pub async fn admin_users(
    pool: &SqlitePool,
    search: Option<&str>,
) -> Result<Vec<AdminUser>, DbError> {
    // A LIKE on a lowered pattern keeps the SQLite matching case-insensitive for ASCII even
    // though the column's own collation is NOCASE only for equality.
    let pattern = search
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .map(|term| format!("%{}%", term.to_lowercase()));
    let rows = sqlx::query(
        "SELECT u.id, u.email, u.created_at, u.is_active, u.is_verified, u.is_superuser,
                u.is_curator, u.quota_mb,
                (SELECT COUNT(*) FROM membership m
                 WHERE m.user_id = u.id AND m.role = 'owner') AS rooms_owned
         FROM \"user\" u
         WHERE ?1 IS NULL OR LOWER(u.email) LIKE ?1
         ORDER BY u.email COLLATE NOCASE",
    )
    .bind(pattern)
    .fetch_all(pool)
    .await?;
    let mut users = Vec::with_capacity(rows.len());
    for row in rows {
        users.push(AdminUser {
            id: {
                let id: String = row.try_get("id")?;
                Uuid::parse_str(&id).map_err(|error| sqlx::Error::Decode(Box::new(error)))?
            },
            email: row.try_get("email")?,
            created_at: row.try_get("created_at")?,
            is_active: row.try_get("is_active")?,
            is_verified: row.try_get("is_verified")?,
            is_superuser: row.try_get("is_superuser")?,
            is_curator: row.try_get("is_curator")?,
            rooms_owned: row.try_get::<i64, _>("rooms_owned")?.max(0) as u64,
            quota_mb: row
                .try_get::<Option<i64>, _>("quota_mb")?
                .and_then(|mb| u64::try_from(mb).ok()),
        });
    }
    Ok(users)
}

/// One room as the admin pages read it, without its size (that comes from the file).
pub struct AdminRoom {
    pub id: String,
    pub name: String,
    pub owner: Option<String>,
    pub members: u64,
    pub limit_mb: Option<u64>,
    pub public: bool,
}

pub async fn admin_rooms(pool: &SqlitePool) -> Result<Vec<AdminRoom>, DbError> {
    let rows = sqlx::query(
        "SELECT r.id, r.name, r.limit_mb, r.public,
                (SELECT u.email FROM membership m JOIN \"user\" u ON u.id = m.user_id
                 WHERE m.room_id = r.id AND m.role = 'owner') AS owner,
                (SELECT COUNT(*) FROM membership m WHERE m.room_id = r.id) AS members
         FROM room r
         ORDER BY r.id",
    )
    .fetch_all(pool)
    .await?;
    let mut rooms = Vec::with_capacity(rows.len());
    for row in rows {
        rooms.push(AdminRoom {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            owner: row.try_get("owner")?,
            members: row.try_get::<i64, _>("members")?.max(0) as u64,
            public: row.try_get("public")?,
            limit_mb: row
                .try_get::<Option<i64>, _>("limit_mb")?
                .and_then(|mb| u64::try_from(mb).ok()),
        });
    }
    Ok(rooms)
}

/// The account counts the admin overview shows: total, active, unverified, superusers and the
/// accounts created in the last seven days.
pub async fn admin_user_counts(pool: &SqlitePool) -> Result<(u64, u64, u64, u64, u64), DbError> {
    let row = sqlx::query(
        "SELECT COUNT(*) AS total,
                COALESCE(SUM(is_active), 0) AS active,
                COALESCE(SUM(NOT is_verified), 0) AS unverified,
                COALESCE(SUM(is_superuser), 0) AS superusers,
                COALESCE(SUM(created_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-7 days')), 0)
                    AS recent
         FROM \"user\"",
    )
    .fetch_one(pool)
    .await?;
    Ok((
        row.try_get::<i64, _>("total")?.max(0) as u64,
        row.try_get::<i64, _>("active")?.max(0) as u64,
        row.try_get::<i64, _>("unverified")?.max(0) as u64,
        row.try_get::<i64, _>("superusers")?.max(0) as u64,
        row.try_get::<i64, _>("recent")?.max(0) as u64,
    ))
}

pub async fn count_rooms(pool: &SqlitePool) -> Result<u64, DbError> {
    let rooms: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM room")
        .fetch_one(pool)
        .await?;
    Ok(rooms.max(0) as u64)
}

pub async fn create_user(
    pool: &SqlitePool,
    email: &str,
    password: &str,
    is_superuser: bool,
    is_verified: bool,
) -> Result<Uuid, DbError> {
    let id = Uuid::new_v4();
    let hashed_password = hash_password(password).await?;
    insert_user(pool, id, email, &hashed_password, is_superuser, is_verified).await?;
    Ok(id)
}

/// Insert a user row wherever the caller is writing: its own pool, or the transaction that also
/// creates the account an OpenID Connect sign-in just proved.
pub async fn insert_user<'e, E: Executor<'e, Database = Sqlite>>(
    executor: E,
    id: Uuid,
    email: &str,
    hashed_password: &str,
    is_superuser: bool,
    is_verified: bool,
) -> Result<(), DbError> {
    sqlx::query(
        r#"INSERT INTO "user" (id, email, hashed_password, is_active, is_verified, is_superuser)
           VALUES (?, ?, ?, TRUE, ?, ?)"#,
    )
    .bind(id.to_string())
    .bind(email)
    .bind(hashed_password)
    .bind(is_verified)
    .bind(is_superuser)
    .execute(executor)
    .await
    .map_err(|error| taken_email(error, email))?;
    Ok(())
}

/// The account a provider subject already signs in as, if the visitor linked one.
pub async fn oauth_user<'e, E: Executor<'e, Database = Sqlite>>(
    executor: E,
    provider: &str,
    subject: &str,
) -> Result<Option<User>, DbError> {
    let row = sqlx::query(
        r#"SELECT "user".id, "user".email, "user".hashed_password, "user".is_active,
                  "user".is_superuser, "user".is_verified
           FROM oauth_account JOIN "user" ON "user".id = oauth_account.user_id
           WHERE oauth_account.provider = ? AND oauth_account.subject = ?"#,
    )
    .bind(provider)
    .bind(subject)
    .fetch_optional(executor)
    .await?;
    Ok(row.as_ref().map(user_from_row).transpose()?)
}

pub async fn link_oauth_account<'e, E: Executor<'e, Database = Sqlite>>(
    executor: E,
    provider: &str,
    subject: &str,
    user_id: Uuid,
) -> Result<(), DbError> {
    sqlx::query(r#"INSERT INTO oauth_account (provider, subject, user_id) VALUES (?, ?, ?)"#)
        .bind(provider)
        .bind(subject)
        .bind(user_id.to_string())
        .execute(executor)
        .await?;
    Ok(())
}

pub async fn linked_providers<'e, E: Executor<'e, Database = Sqlite>>(
    executor: E,
    user_id: Uuid,
) -> Result<Vec<String>, DbError> {
    let rows =
        sqlx::query(r#"SELECT provider FROM oauth_account WHERE user_id = ? ORDER BY provider"#)
            .bind(user_id.to_string())
            .fetch_all(executor)
            .await?;
    rows.iter()
        .map(|row| row.try_get("provider").map_err(DbError::from))
        .collect()
}

/// Write every mutable column of `user`. The address is unique, so a duplicate is reported as
/// one; the caller decides whether that is a conflict or a bad request.
pub async fn save_user<'e, E: Executor<'e, Database = Sqlite>>(
    executor: E,
    user: &User,
) -> Result<(), DbError> {
    sqlx::query(
        r#"UPDATE "user"
           SET email = ?, hashed_password = ?, is_active = ?, is_superuser = ?, is_verified = ?
           WHERE id = ?"#,
    )
    .bind(&user.email)
    .bind(&user.hashed_password)
    .bind(user.is_active)
    .bind(user.is_superuser)
    .bind(user.is_verified)
    .bind(user.id.to_string())
    .execute(executor)
    .await
    .map_err(|error| taken_email(error, &user.email))?;
    Ok(())
}

/// Remove the account. Memberships go with it (the schema cascades) and the rooms' files stay,
/// like Python's delete, so an owned room survives its last member.
pub async fn delete_user(
    executor: impl Executor<'_, Database = Sqlite>,
    id: Uuid,
) -> Result<bool, DbError> {
    let deleted = sqlx::query(r#"DELETE FROM "user" WHERE id = ?"#)
        .bind(id.to_string())
        .execute(executor)
        .await?;
    Ok(deleted.rows_affected() > 0)
}

/// Hash on the blocking pool: argon2 wants 19 MB and tens of milliseconds, which must not
/// park an async worker while other requests wait.
pub async fn hash_password(password: &str) -> Result<String, DbError> {
    let password = password.to_owned();
    crate::hashing::off_worker(move || hash_password_blocking(&password))
        .await
        .map_err(DbError::from)?
}

fn hash_password_blocking(password: &str) -> Result<String, DbError> {
    Ok(Argon2::default()
        .hash_password(password.as_bytes())?
        .to_string())
}

/// Confirm an account. Returns whether a row matched, so a link for a deleted account is a
/// bad token rather than a silent success.
pub async fn set_verified(pool: &SqlitePool, id: Uuid) -> Result<bool, DbError> {
    let updated = sqlx::query(r#"UPDATE "user" SET is_verified = TRUE WHERE id = ?"#)
        .bind(id.to_string())
        .execute(pool)
        .await?;
    Ok(updated.rows_affected() > 0)
}

/// Replace a password whose hash is still `expected` with `new_hash`. The argon2 hash is what
/// a reset link's fingerprint is compared against, so comparing it inside the write is also
/// what spends the link: of two requests holding the same link only the first finds the old
/// hash still there.
pub async fn replace_password<'e, E: Executor<'e, Database = Sqlite>>(
    executor: E,
    id: Uuid,
    expected: &str,
    new_hash: &str,
) -> Result<bool, DbError> {
    let updated = sqlx::query(
        r#"UPDATE "user" SET hashed_password = ? WHERE id = ? AND hashed_password = ?"#,
    )
    .bind(new_hash)
    .bind(id.to_string())
    .bind(expected)
    .execute(executor)
    .await?;
    Ok(updated.rows_affected() > 0)
}

/// How long a browser session lasts, and how long a token lasts unused.
const BROWSER_LIFETIME: &str = "+3600 seconds";
const TOKEN_IDLE_LIFETIME: &str = "+6 months";

/// Record a browser session for the cookie value whose hash this is. Expired sessions of the
/// same account go in the same transaction, so the table only holds sessions someone still has.
pub async fn create_session(
    pool: &SqlitePool,
    token_hash: &str,
    user_id: Uuid,
) -> Result<(), DbError> {
    insert_session(pool, token_hash, user_id, None, BROWSER_LIFETIME).await
}

/// Record a token an app signs in with, under the name the account page shows.
pub async fn create_token(
    pool: &SqlitePool,
    token_hash: &str,
    user_id: Uuid,
    name: &str,
) -> Result<(), DbError> {
    insert_session(pool, token_hash, user_id, Some(name), TOKEN_IDLE_LIFETIME).await
}

async fn insert_session(
    pool: &SqlitePool,
    token_hash: &str,
    user_id: Uuid,
    name: Option<&str>,
    lifetime: &str,
) -> Result<(), DbError> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM session WHERE user_id = ? AND expires_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now')")
    .bind(user_id.to_string())
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO session (token_hash, id, user_id, name, created_at, last_used_at, expires_at)
         VALUES (?, ?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?))")
    .bind(token_hash)
    .bind(Uuid::new_v4().simple().to_string())
    .bind(user_id.to_string())
    .bind(name)
    .bind(lifetime)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Who a live session belongs to, and whether it is a token whose last use is over an hour
/// old: the caller then moves its expiry, so a token in steady use writes once an hour at most.
#[derive(Debug)]
pub struct SessionHolder {
    pub user: User,
    pub stale: bool,
}

/// The account a live session belongs to. An expired or deleted session is the same miss as an
/// unknown token, so callers have one refusal to answer.
pub async fn session_user(
    pool: &SqlitePool,
    token_hash: &str,
) -> Result<Option<SessionHolder>, DbError> {
    let row = sqlx::query(r#"SELECT "user".id, "user".email, "user".hashed_password, "user".is_active,
                  "user".is_superuser, "user".is_verified,
                  session.name IS NOT NULL
                    AND session.last_used_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-3600 seconds')
                    AS stale
           FROM session JOIN "user" ON "user".id = session.user_id
           WHERE session.token_hash = ? AND session.expires_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"#)
    .bind(token_hash)
    .fetch_optional(pool)
    .await?;
    let holder = row.map(|row| {
        Ok::<_, sqlx::Error>(SessionHolder {
            user: user_from_row(&row)?,
            stale: row.try_get("stale")?,
        })
    });
    Ok(holder.transpose()?)
}

/// Mark a token used now and give it six more months.
pub async fn touch_token(pool: &SqlitePool, token_hash: &str) -> Result<(), DbError> {
    sqlx::query("UPDATE session
         SET last_used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), expires_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?)
         WHERE token_hash = ? AND name IS NOT NULL")
    .bind(TOKEN_IDLE_LIFETIME)
    .bind(token_hash)
    .execute(pool)
    .await?;
    Ok(())
}

/// A token as the account page and `GET /api/tokens` show it: never the token itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRow {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub last_used_at: String,
}

/// The account's live tokens, newest first.
pub async fn list_tokens(pool: &SqlitePool, user_id: Uuid) -> Result<Vec<TokenRow>, DbError> {
    let rows = sqlx::query("SELECT id, name, created_at, last_used_at FROM session
         WHERE user_id = ? AND name IS NOT NULL AND expires_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         ORDER BY created_at DESC, id")
    .bind(user_id.to_string())
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok::<_, DbError>(TokenRow {
                id: row.try_get("id")?,
                name: row.try_get("name")?,
                created_at: row.try_get("created_at")?,
                last_used_at: row.try_get("last_used_at")?,
            })
        })
        .collect()
}

/// Drop one of the account's tokens by its id, answering the hash it had (so its sockets can
/// be closed) or `None` when the account has no such token.
pub async fn delete_token(
    pool: &SqlitePool,
    user_id: Uuid,
    id: &str,
) -> Result<Option<(String, String)>, DbError> {
    let row = sqlx::query(
        "DELETE FROM session WHERE user_id = ? AND id = ? AND name IS NOT NULL
         RETURNING token_hash, name",
    )
    .bind(user_id.to_string())
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let deleted =
        row.map(|row| Ok::<_, sqlx::Error>((row.try_get("token_hash")?, row.try_get("name")?)));
    Ok(deleted.transpose()?)
}

/// Drop one session; a token that is not there is not an error, since the cookie is going away
/// either way.
pub async fn delete_session(pool: &SqlitePool, token_hash: &str) -> Result<(), DbError> {
    sqlx::query("DELETE FROM session WHERE token_hash = ?")
        .bind(token_hash)
        .execute(pool)
        .await?;
    Ok(())
}

/// Drop every session and token of an account, for the writes that must leave nothing
/// working, such as a deactivation or a password reset; answers their hashes, so the
/// caller can close what they opened.
pub async fn end_sessions_of<'e, E: Executor<'e, Database = Sqlite>>(
    executor: E,
    user_id: Uuid,
) -> Result<Vec<String>, DbError> {
    Ok(
        sqlx::query_scalar("DELETE FROM session WHERE user_id = ? RETURNING token_hash")
            .bind(user_id.to_string())
            .fetch_all(executor)
            .await?,
    )
}

/// Drop the account's browser sessions and keep its tokens: after a password change the
/// person sees their signed-in apps and revokes them on purpose.
pub async fn end_browser_sessions_of<'e, E: Executor<'e, Database = Sqlite>>(
    executor: E,
    user_id: Uuid,
) -> Result<(), DbError> {
    sqlx::query("DELETE FROM session WHERE user_id = ? AND name IS NULL")
        .bind(user_id.to_string())
        .execute(executor)
        .await?;
    Ok(())
}

/// The address column is unique, case-insensitively: a second account for the same person is
/// a request-level conflict, not an internal failure.
fn taken_email(error: sqlx::Error, email: &str) -> DbError {
    match &error {
        sqlx::Error::Database(source) if source.is_unique_violation() => DbError::EmailTaken {
            email: email.to_owned(),
        },
        _ => DbError::Sqlx(error),
    }
}

pub async fn create_room(
    pool: &SqlitePool,
    owner_id: Uuid,
    room_id: &str,
    name: &str,
) -> Result<(), DbError> {
    if cocat_core::room_id::is_public_id(room_id) {
        return Err(DbError::ReservedId(room_id.to_owned()));
    }
    let mut tx = pool.begin().await?;
    insert_room(&mut tx, owner_id, room_id, name).await?;
    tx.commit().await?;
    Ok(())
}

async fn insert_room(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    owner_id: Uuid,
    room_id: &str,
    name: &str,
) -> Result<(), DbError> {
    validate_room_id(room_id)?;
    sqlx::query("INSERT INTO room (id, name, created_by) VALUES (?, ?, ?)")
        .bind(room_id)
        .bind(name)
        .bind(owner_id.to_string())
        .execute(&mut **tx)
        .await?;
    insert_membership(tx, room_id, owner_id, Role::Owner).await?;
    Ok(())
}

/// The name of the room every account owns from the moment it exists. Python cocat named
/// it "Personal"; rooms imported from Python keep that name.
pub const PERSONAL_ROOM_NAME: &str = "My catalogues";

/// Python's `create_personal_room`: a bare-uuid room named [`PERSONAL_ROOM_NAME`] whose
/// owner is `user`, recorded as the account's own project. Registration and `create-user`
/// both run it.
pub async fn create_personal_room(pool: &SqlitePool, user: Uuid) -> Result<(), DbError> {
    let room_id = Uuid::new_v4().simple().to_string();
    let mut tx = pool.begin().await?;
    insert_room(&mut tx, user, &room_id, PERSONAL_ROOM_NAME).await?;
    sqlx::query(r#"UPDATE "user" SET personal_room = ? WHERE id = ?"#)
        .bind(&room_id)
        .bind(user.to_string())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Migration 0010's backfill, for accounts imported after it ran: each unmarked account's
/// oldest owned uuid-hex room named like an own project. Keep it the same as the migration.
pub const MARK_PERSONAL_ROOMS: &str = r#"UPDATE "user" SET personal_room = (
    SELECT room.id
    FROM room JOIN membership ON membership.room_id = room.id
    WHERE membership.user_id = "user".id
      AND membership.role = 'owner'
      AND room.name IN ('My catalogues', 'Personal')
      AND length(room.id) = 32
      AND room.id NOT GLOB '*[^0-9a-f]*'
    ORDER BY room.created_at, room.id
    LIMIT 1
)
WHERE personal_room IS NULL"#;

/// Migration 0011, for accounts imported after it ran: an unmarked account's own room is the
/// one it owns named by its email's local part. Run after [`MARK_PERSONAL_ROOMS`].
pub const MARK_EMAIL_ROOMS: &str = include_str!("../migrations/0011_email_personal_room.sql");

/// The account's own project, while the account still owns it.
pub async fn personal_room(
    executor: impl Executor<'_, Database = Sqlite>,
    user: Uuid,
) -> Result<Option<String>, DbError> {
    Ok(sqlx::query_scalar(
        r#"SELECT "user".personal_room FROM "user"
           JOIN membership ON membership.room_id = "user".personal_room
             AND membership.user_id = "user".id AND membership.role = 'owner'
           WHERE "user".id = ?"#,
    )
    .bind(user.to_string())
    .fetch_optional(executor)
    .await?)
}

/// A public catalogue as the index lists it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct PublicRoom {
    pub id: String,
    pub name: String,
    pub folder: Option<String>,
    pub source: Option<String>,
    pub description: Option<String>,
    /// How many events it held when last counted, from the room file `counted_file` names.
    pub public_count: Option<i64>,
    pub counted_file: Option<String>,
}

/// Makes a public catalogue: no owner and no members, so `created_by` only records who.
/// Makes the public room; false when a room has its id already. The insert itself tells, so
/// two callers racing for one id cannot both get it.
pub async fn create_public_room(
    pool: &SqlitePool,
    room: &PublicRoom,
    by: Uuid,
) -> Result<bool, DbError> {
    let made = sqlx::query(
        "INSERT INTO room (id, name, created_by, public, folder, source, description)
         VALUES (?, ?, ?, TRUE, ?, ?, ?)",
    )
    .bind(&room.id)
    .bind(&room.name)
    .bind(by.to_string())
    .bind(&room.folder)
    .bind(&room.source)
    .bind(&room.description)
    .execute(pool)
    .await;
    match made {
        Ok(_) => Ok(true),
        Err(sqlx::Error::Database(error)) if error.is_unique_violation() => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Every public catalogue, by folder then name.
pub async fn public_rooms(pool: &SqlitePool) -> Result<Vec<PublicRoom>, DbError> {
    Ok(sqlx::query_as(
        "SELECT id, name, folder, source, description, public_count, counted_file FROM room
         WHERE public ORDER BY folder IS NULL, folder, name, id",
    )
    .fetch_all(pool)
    .await?)
}

pub async fn public_room(pool: &SqlitePool, room_id: &str) -> Result<Option<PublicRoom>, DbError> {
    Ok(sqlx::query_as(
        "SELECT id, name, folder, source, description, public_count, counted_file FROM room
         WHERE public AND id = ?",
    )
    .bind(room_id)
    .fetch_optional(pool)
    .await?)
}

/// Writes what the index says about a public catalogue: its name, folder, source and
/// description.
pub async fn update_public_room(pool: &SqlitePool, room: &PublicRoom) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE room SET name = ?, folder = ?, source = ?, description = ? WHERE public AND id = ?",
    )
    .bind(&room.name)
    .bind(&room.folder)
    .bind(&room.source)
    .bind(&room.description)
    .bind(&room.id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn store_public_count(
    pool: &SqlitePool,
    room_id: &str,
    count: i64,
    counted_file: &str,
) -> Result<(), DbError> {
    sqlx::query("UPDATE room SET public_count = ?, counted_file = ? WHERE public AND id = ?")
        .bind(count)
        .bind(counted_file)
        .bind(room_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn is_public(pool: &SqlitePool, room_id: &str) -> Result<bool, DbError> {
    let public: Option<bool> = sqlx::query_scalar("SELECT public FROM room WHERE id = ?")
        .bind(room_id)
        .fetch_optional(pool)
        .await?;
    Ok(public.unwrap_or(false))
}

/// Refuses a database where a project that is not public has a public catalogue's id: it
/// came from Python, and serving it would hand its catalogues to everyone signed in.
pub async fn refuse_reserved_ids(
    executor: impl Executor<'_, Database = Sqlite>,
) -> Result<(), DbError> {
    let clashing: Vec<String> =
        sqlx::query_scalar("SELECT id FROM room WHERE NOT public AND substr(id, 1, ?) = ?")
            .bind(cocat_core::room_id::PUBLIC_PREFIX.len() as i64)
            .bind(cocat_core::room_id::PUBLIC_PREFIX)
            .fetch_all(executor)
            .await?;
    match clashing.is_empty() {
        true => Ok(()),
        false => Err(DbError::ReservedIds(clashing)),
    }
}

pub async fn rename_room(pool: &SqlitePool, room_id: &str, name: &str) -> Result<bool, DbError> {
    let updated = sqlx::query("UPDATE room SET name = ? WHERE id = ?")
        .bind(name)
        .bind(room_id)
        .execute(pool)
        .await?;
    Ok(updated.rows_affected() > 0)
}

/// Whether the room's editors may share it, or `None` for an unknown room.
pub async fn editors_can_share(pool: &SqlitePool, room_id: &str) -> Result<Option<bool>, DbError> {
    Ok(
        sqlx::query_scalar("SELECT editors_can_share FROM room WHERE id = ?")
            .bind(room_id)
            .fetch_optional(pool)
            .await?,
    )
}

pub async fn set_editors_can_share(
    pool: &SqlitePool,
    room_id: &str,
    allowed: bool,
) -> Result<bool, DbError> {
    let updated = sqlx::query("UPDATE room SET editors_can_share = ? WHERE id = ?")
        .bind(allowed)
        .bind(room_id)
        .execute(pool)
        .await?;
    Ok(updated.rows_affected() > 0)
}

/// The rooms `user_id` reaches whose owner keeps sharing to themselves.
pub async fn rooms_closed_to_editors(
    pool: &SqlitePool,
    user_id: Uuid,
) -> Result<Vec<String>, DbError> {
    Ok(sqlx::query_scalar(
        "SELECT room.id FROM room
         WHERE room.editors_can_share = FALSE
           AND (room.id IN (SELECT room_id FROM membership WHERE user_id = ?)
             OR room.id IN (SELECT room_group.room_id
                            FROM room_group
                              JOIN group_member ON group_member.group_id = room_group.group_id
                            WHERE group_member.user_id = ?))",
    )
    .bind(user_id.to_string())
    .bind(user_id.to_string())
    .fetch_all(pool)
    .await?)
}

pub async fn delete_room(pool: &SqlitePool, room_id: &str) -> Result<bool, DbError> {
    let deleted = sqlx::query("DELETE FROM room WHERE id = ?")
        .bind(room_id)
        .execute(pool)
        .await?;
    Ok(deleted.rows_affected() > 0)
}

pub async fn add_member(
    pool: &SqlitePool,
    room_id: &str,
    user_id: Uuid,
    role: Role,
) -> Result<Role, DbError> {
    let mut tx = pool.begin().await?;
    let granted = raise_membership(&mut tx, room_id, user_id, role).await?;
    tx.commit().await?;
    Ok(granted)
}

/// Adds `user_id` to the room on the caller's transaction, keeping a higher role than `role`
/// rather than lowering it, and returns the role they hold afterwards. It takes the caller's
/// transaction so joining a room and spending the invitation that admitted the member commit
/// or roll back together.
pub async fn raise_membership(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    room_id: &str,
    user_id: Uuid,
    role: Role,
) -> Result<Role, DbError> {
    let current = membership_role(tx, room_id, user_id).await?;
    Ok(match current {
        Some(existing) if existing.rank() >= role.rank() => existing,
        Some(_) => {
            set_membership_role(tx, room_id, user_id, role).await?;
            role
        }
        None => {
            insert_membership(tx, room_id, user_id, role).await?;
            role
        }
    })
}

pub async fn remove_member(
    pool: &SqlitePool,
    room_id: &str,
    user_id: Uuid,
) -> Result<bool, DbError> {
    let removed = sqlx::query("DELETE FROM membership WHERE room_id = ? AND user_id = ?")
        .bind(room_id)
        .bind(user_id.to_string())
        .execute(pool)
        .await?;
    Ok(removed.rows_affected() > 0)
}

pub async fn set_role(
    pool: &SqlitePool,
    room_id: &str,
    user_id: Uuid,
    role: Role,
) -> Result<bool, DbError> {
    let updated = sqlx::query("UPDATE membership SET role = ? WHERE room_id = ? AND user_id = ?")
        .bind(role.as_str())
        .bind(room_id)
        .bind(user_id.to_string())
        .execute(pool)
        .await?;
    Ok(updated.rows_affected() > 0)
}

pub async fn set_owner(pool: &SqlitePool, room_id: &str, user_id: Uuid) -> Result<(), DbError> {
    let mut tx = pool.begin().await?;
    // one_owner_per_room allows a single owner row, so the old owner steps down first.
    sqlx::query(
        "UPDATE membership SET role = 'editor'
         WHERE room_id = ? AND role = 'owner' AND user_id != ?",
    )
    .bind(room_id)
    .bind(user_id.to_string())
    .execute(&mut *tx)
    .await?;
    let promoted =
        sqlx::query("UPDATE membership SET role = 'owner' WHERE room_id = ? AND user_id = ?")
            .bind(room_id)
            .bind(user_id.to_string())
            .execute(&mut *tx)
            .await?;
    if promoted.rows_affected() == 0 {
        return Err(DbError::NoSuchMembership {
            room_id: room_id.to_owned(),
            user_id,
        });
    }
    tx.commit().await?;
    Ok(())
}

/// Names the version whose records are `records`, replacing the name it had. The name is
/// stored under its first record; a row under any other of its records (rows written when a
/// version was keyed by its last record) goes. The 0008 migration's comment still says a
/// version is its last record: sqlx refuses an applied migration whose checksum changed, so
/// that file stays as it shipped.
pub async fn name_version(
    pool: &SqlitePool,
    room_id: &str,
    records: RangeInclusive<i64>,
    name: &str,
    by: Uuid,
) -> Result<(), DbError> {
    let mut tx = begin_immediate(pool).await?;
    put_version_name(&mut tx, room_id, &records, name, by).await?;
    tx.commit().await?;
    Ok(())
}

async fn put_version_name(
    tx: &mut Transaction<'_, Sqlite>,
    room_id: &str,
    records: &RangeInclusive<i64>,
    name: &str,
    by: Uuid,
) -> Result<(), sqlx::Error> {
    delete_version_names(tx, room_id, records).await?;
    sqlx::query(
        "INSERT INTO version_name (room_id, version, name, created_by) VALUES (?, ?, ?, ?)",
    )
    .bind(room_id)
    .bind(records.start())
    .bind(name)
    .bind(by.to_string())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// A version someone saves: its records, the last one being the room's last record then.
pub struct SavedVersion<'a> {
    pub records: RangeInclusive<i64>,
    pub name: Option<&'a str>,
    pub message: Option<&'a str>,
    /// The newest save the version's grouping knew of.
    pub after: Option<i64>,
}

/// Marks the end of `saved` and names it. False, and nothing stored, when the room has a
/// newer save than the one the grouping knew: then nothing was written since, or the
/// version's first record moved under a concurrent save.
pub async fn save_version(
    pool: &SqlitePool,
    room_id: &str,
    saved: &SavedVersion<'_>,
    by: Uuid,
) -> Result<bool, DbError> {
    let mut tx = begin_immediate(pool).await?;
    let newest: Option<i64> =
        sqlx::query_scalar("SELECT MAX(record) FROM version_commit WHERE room_id = ?")
            .bind(room_id)
            .fetch_one(&mut *tx)
            .await?;
    if newest != saved.after || newest >= Some(*saved.records.end()) {
        return Ok(false);
    }
    sqlx::query(
        "INSERT INTO version_commit (room_id, record, message, created_by) VALUES (?, ?, ?, ?)",
    )
    .bind(room_id)
    .bind(saved.records.end())
    .bind(saved.message)
    .bind(by.to_string())
    .execute(&mut *tx)
    .await?;
    if let Some(name) = saved.name {
        put_version_name(&mut tx, room_id, &saved.records, name, by).await?;
    }
    tx.commit().await?;
    Ok(true)
}

/// The notes of a room's saved versions, by the record each ends at.
pub async fn version_commits(
    pool: &SqlitePool,
    room_id: &str,
) -> Result<BTreeMap<i64, Option<String>>, DbError> {
    let rows: Vec<(i64, Option<String>)> =
        sqlx::query_as("SELECT record, message FROM version_commit WHERE room_id = ?")
            .bind(room_id)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().collect())
}

pub async fn unname_version(
    pool: &SqlitePool,
    room_id: &str,
    records: RangeInclusive<i64>,
) -> Result<(), DbError> {
    let mut tx = begin_immediate(pool).await?;
    delete_version_names(&mut tx, room_id, &records).await?;
    tx.commit().await?;
    Ok(())
}

async fn delete_version_names(
    tx: &mut Transaction<'_, Sqlite>,
    room_id: &str,
    records: &RangeInclusive<i64>,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM version_name WHERE room_id = ? AND version BETWEEN ? AND ?")
        .bind(room_id)
        .bind(records.start())
        .bind(records.end())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// The names of a room's versions, by the record each is stored under.
pub async fn version_names(
    pool: &SqlitePool,
    room_id: &str,
) -> Result<BTreeMap<i64, String>, DbError> {
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT version, name FROM version_name WHERE room_id = ?")
            .bind(room_id)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().collect())
}

async fn insert_membership(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    room_id: &str,
    user_id: Uuid,
    role: Role,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO membership (room_id, user_id, role) VALUES (?, ?, ?)")
        .bind(room_id)
        .bind(user_id.to_string())
        .bind(role.as_str())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn membership_role(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    room_id: &str,
    user_id: Uuid,
) -> Result<Option<Role>, DbError> {
    let role: Option<String> =
        sqlx::query("SELECT role FROM membership WHERE room_id = ? AND user_id = ?")
            .bind(room_id)
            .bind(user_id.to_string())
            .fetch_optional(&mut **tx)
            .await?
            .map(|row| row.try_get("role"))
            .transpose()?;
    Ok(role.as_deref().map(role_from_name).transpose()?)
}

async fn set_membership_role(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    room_id: &str,
    user_id: Uuid,
    role: Role,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE membership SET role = ? WHERE room_id = ? AND user_id = ?")
        .bind(role.as_str())
        .bind(room_id)
        .bind(user_id.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

fn role_from_name(name: &str) -> Result<Role, sqlx::Error> {
    Role::from_str(name).map_err(|error| sqlx::Error::Decode(Box::new(error)))
}

fn user_from_row(row: &SqliteRow) -> Result<User, sqlx::Error> {
    let id: String = row.try_get("id")?;
    Ok(User {
        id: Uuid::parse_str(&id).map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
        email: row.try_get("email")?,
        hashed_password: row.try_get("hashed_password")?,
        is_active: row.try_get("is_active")?,
        is_superuser: row.try_get("is_superuser")?,
        is_verified: row.try_get("is_verified")?,
    })
}

#[cfg(test)]
mod tests {
    /// The workspace builds the hashing crates optimised in tests to keep them fast; this
    /// pins that the stored hash keeps argon2's full-strength defaults, so speeding tests up
    /// can never be done by weakening the hash.
    #[test]
    fn passwords_are_stored_with_full_strength_argon2id() {
        let hash = super::hash_password_blocking("correct horse battery staple").unwrap();
        assert!(
            hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
            "unexpected hash parameters: {hash}"
        );
    }
}
