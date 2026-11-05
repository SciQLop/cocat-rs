//! The server's SQLite schema, applied by sqlx migrations, and the queries on it.

use std::collections::BTreeMap;
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
    #[error(transparent)]
    InvalidRoomId(#[from] InvalidRoomId),
    #[error("{user_id} is not a member of room {room_id}")]
    NoSuchMembership { room_id: String, user_id: Uuid },
    #[error("{email} already has an account")]
    EmailTaken { email: String },
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

pub async fn user_by_email(pool: &SqlitePool, email: &str) -> Result<Option<User>, DbError> {
    let row = sqlx::query(
        r#"SELECT id, email, hashed_password, is_active, is_superuser, is_verified
           FROM "user" WHERE email = ?"#,
    )
    .bind(email)
    .fetch_optional(pool)
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
/// only ever comes from a direct membership.
pub async fn role_in(
    pool: &SqlitePool,
    room_id: &str,
    user_id: Uuid,
) -> Result<Option<Role>, DbError> {
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT role FROM membership WHERE room_id = ? AND user_id = ?
         UNION ALL
         SELECT room_group.role
         FROM room_group
           JOIN group_member ON group_member.group_id = room_group.group_id
         WHERE room_group.room_id = ? AND group_member.user_id = ?",
    )
    .bind(room_id)
    .bind(user_id.to_string())
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

pub async fn create_user(
    pool: &SqlitePool,
    email: &str,
    password: &str,
    is_superuser: bool,
    is_verified: bool,
) -> Result<Uuid, DbError> {
    let id = Uuid::new_v4();
    let hashed_password = hash_password(password)?;
    sqlx::query(
        r#"INSERT INTO "user" (id, email, hashed_password, is_active, is_verified, is_superuser)
           VALUES (?, ?, ?, TRUE, ?, ?)"#,
    )
    .bind(id.to_string())
    .bind(email)
    .bind(hashed_password)
    .bind(is_verified)
    .bind(is_superuser)
    .execute(pool)
    .await
    .map_err(|error| taken_email(error, email))?;
    Ok(id)
}

/// Write every mutable column of `user`. The address is unique, so a duplicate is reported as
/// one; the caller decides whether that is a conflict or a bad request.
pub async fn save_user(pool: &SqlitePool, user: &User) -> Result<(), DbError> {
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
    .execute(pool)
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

pub fn hash_password(password: &str) -> Result<String, DbError> {
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

/// Replace a password whose hash is still `expected`. The argon2 hash is what a reset link's
/// fingerprint is compared against, so comparing it inside the write is also what spends the
/// link: of two requests holding the same link only the first finds the old hash still there.
pub async fn replace_password(
    pool: &SqlitePool,
    id: Uuid,
    expected: &str,
    password: &str,
) -> Result<bool, DbError> {
    let updated = sqlx::query(
        r#"UPDATE "user" SET hashed_password = ? WHERE id = ? AND hashed_password = ?"#,
    )
    .bind(hash_password(password)?)
    .bind(id.to_string())
    .bind(expected)
    .execute(pool)
    .await?;
    Ok(updated.rows_affected() > 0)
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
    validate_room_id(room_id)?;
    let mut tx = pool.begin().await?;
    sqlx::query("INSERT INTO room (id, name, created_by) VALUES (?, ?, ?)")
        .bind(room_id)
        .bind(name)
        .bind(owner_id.to_string())
        .execute(&mut *tx)
        .await?;
    insert_membership(&mut tx, room_id, owner_id, Role::Owner).await?;
    tx.commit().await?;
    Ok(())
}

/// What Python (`names.py`) calls the room every account owns from the moment it exists.
pub const PERSONAL_ROOM_NAME: &str = "Personal";

/// Python's `create_personal_room`: a bare-uuid room named [`PERSONAL_ROOM_NAME`] whose
/// owner is `user`. Registration and `create-user` both run it.
pub async fn create_personal_room(pool: &SqlitePool, user: Uuid) -> Result<(), DbError> {
    let room_id = Uuid::new_v4().simple().to_string();
    create_room(pool, user, &room_id, PERSONAL_ROOM_NAME).await
}

pub async fn rename_room(pool: &SqlitePool, room_id: &str, name: &str) -> Result<bool, DbError> {
    let updated = sqlx::query("UPDATE room SET name = ? WHERE id = ?")
        .bind(name)
        .bind(room_id)
        .execute(pool)
        .await?;
    Ok(updated.rows_affected() > 0)
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
