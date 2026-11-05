//! The server's SQLite schema, applied by sqlx migrations, and the queries on it.

use std::path::Path;
use std::str::FromStr;

use argon2::Argon2;
use argon2::password_hash::PasswordHasher;
use sqlx::sqlite::{SqliteConnectOptions, SqliteRow};
use sqlx::{Row, SqlitePool};
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub hashed_password: String,
    pub is_active: bool,
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

pub async fn user_by_email(pool: &SqlitePool, email: &str) -> Result<Option<User>, DbError> {
    let row =
        sqlx::query(r#"SELECT id, email, hashed_password, is_active FROM "user" WHERE email = ?"#)
            .bind(email)
            .fetch_optional(pool)
            .await?;
    Ok(row.as_ref().map(user_from_row).transpose()?)
}

pub async fn user_by_id(pool: &SqlitePool, id: Uuid) -> Result<Option<User>, DbError> {
    let row =
        sqlx::query(r#"SELECT id, email, hashed_password, is_active FROM "user" WHERE id = ?"#)
            .bind(id.to_string())
            .fetch_optional(pool)
            .await?;
    Ok(row.as_ref().map(user_from_row).transpose()?)
}

pub async fn role_in(
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

pub async fn create_user(
    pool: &SqlitePool,
    email: &str,
    password: &str,
    is_superuser: bool,
) -> Result<Uuid, DbError> {
    let id = Uuid::new_v4();
    let hashed_password = Argon2::default()
        .hash_password(password.as_bytes())?
        .to_string();
    sqlx::query(
        r#"INSERT INTO "user" (id, email, hashed_password, is_active, is_verified, is_superuser)
           VALUES (?, ?, ?, TRUE, TRUE, ?)"#,
    )
    .bind(id.to_string())
    .bind(email)
    .bind(hashed_password)
    .bind(is_superuser)
    .execute(pool)
    .await?;
    Ok(id)
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
    let current = membership_role(&mut tx, room_id, user_id).await?;
    let resulting = match current {
        Some(existing) if existing.rank() >= role.rank() => existing,
        Some(_) => {
            set_membership_role(&mut tx, room_id, user_id, role).await?;
            role
        }
        None => {
            insert_membership(&mut tx, room_id, user_id, role).await?;
            role
        }
    };
    tx.commit().await?;
    Ok(resulting)
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
    })
}
