//! The Python server's SQLite database, read-only for the server and writable for seeding.

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

// simplify: the spike mirrors the Python schema; phase 1 owns it with sqlx migrations.
const SCHEMA: [&str; 3] = [
    r#"CREATE TABLE IF NOT EXISTS "user" (
        id CHAR(36) PRIMARY KEY,
        email VARCHAR(320) NOT NULL UNIQUE,
        hashed_password VARCHAR(1024) NOT NULL,
        is_active BOOLEAN NOT NULL,
        is_verified BOOLEAN NOT NULL,
        is_superuser BOOLEAN NOT NULL,
        rooms JSON NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS room (
        id VARCHAR(255) PRIMARY KEY,
        name VARCHAR(255) NOT NULL,
        created_at DATETIME NOT NULL,
        created_by CHAR(36) REFERENCES "user"(id) ON DELETE SET NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS membership (
        room_id VARCHAR(255) NOT NULL REFERENCES room(id) ON DELETE CASCADE,
        user_id CHAR(36) NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
        role VARCHAR(6) NOT NULL CHECK (role IN ('owner', 'editor', 'viewer')),
        joined_at DATETIME NOT NULL,
        PRIMARY KEY (room_id, user_id)
    )"#,
];

#[derive(Debug, Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error("hashing a password: {0}")]
    Hash(#[from] argon2::password_hash::Error),
    #[error(transparent)]
    InvalidRoomId(#[from] InvalidRoomId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub hashed_password: String,
    pub is_active: bool,
}

pub async fn open_read_only(db_path: &Path) -> Result<SqlitePool, DbError> {
    connect(db_path, true).await
}

pub async fn open_writable(db_path: &Path) -> Result<SqlitePool, DbError> {
    connect(db_path, false).await
}

async fn connect(db_path: &Path, read_only: bool) -> Result<SqlitePool, DbError> {
    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .read_only(read_only)
        .create_if_missing(!read_only)
        .foreign_keys(true);
    Ok(SqlitePool::connect_with(options).await?)
}

pub async fn ensure_schema(pool: &SqlitePool) -> Result<(), DbError> {
    for statement in SCHEMA {
        sqlx::query(statement).execute(pool).await?;
    }
    Ok(())
}

pub async fn user_by_email(pool: &SqlitePool, email: &str) -> Result<Option<User>, DbError> {
    let row =
        sqlx::query(r#"SELECT id, email, hashed_password, is_active FROM "user" WHERE email = ?"#)
            .bind(email)
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
    role.map(|role| Role::from_str(&role).map_err(|error| sqlx::Error::Decode(Box::new(error))))
        .transpose()
        .map_err(DbError::from)
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
        r#"INSERT INTO "user" (id, email, hashed_password, is_active, is_verified, is_superuser, rooms)
           VALUES (?, ?, ?, TRUE, TRUE, ?, '[]')"#,
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
    sqlx::query(
        "INSERT INTO room (id, name, created_at, created_by) VALUES (?, ?, datetime('now'), ?)",
    )
    .bind(room_id)
    .bind(name)
    .bind(owner_id.to_string())
    .execute(&mut *tx)
    .await?;
    insert_membership(&mut tx, room_id, owner_id, Role::Owner).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn add_member(
    pool: &SqlitePool,
    room_id: &str,
    user_id: Uuid,
    role: Role,
) -> Result<(), DbError> {
    let mut tx = pool.begin().await?;
    insert_membership(&mut tx, room_id, user_id, role).await?;
    tx.commit().await?;
    Ok(())
}

async fn insert_membership(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    room_id: &str,
    user_id: Uuid,
    role: Role,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT OR REPLACE INTO membership (room_id, user_id, role, joined_at)
         VALUES (?, ?, ?, datetime('now'))",
    )
    .bind(room_id)
    .bind(user_id.to_string())
    .bind(role.as_str())
    .execute(&mut **tx)
    .await?;
    Ok(())
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
