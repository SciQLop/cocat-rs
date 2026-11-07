//! Import a running Python cocat deployment into this server's database, and move its
//! room files to layout v2.
//!
//! The Python database is only read. A room file still in layout v1 gets one more record,
//! the update that rewrites it in v2, so a second run changes nothing; Python cocat cannot
//! read a migrated room.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use bytes::Bytes;
use cocat_core::model;
use cocat_core::store::{FileRoomStore, StoreError};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use thiserror::Error;

use time::format_description::BorrowedFormatItem;
use time::macros::format_description;
use time::{OffsetDateTime, PrimitiveDateTime};
use yrs::updates::decoder::Decode;
use yrs::{Doc, Transact, Update};

use crate::db;

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot read the Python database {path}: {source}")]
    Python {
        path: PathBuf,
        #[source]
        source: sqlx::Error,
    },
    #[error(transparent)]
    Db(#[from] db::DbError),
    #[error("cannot write the database: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("--db and --users name the same file")]
    SameFile,
    #[error(
        "without --users, --owner must name the account that owns the rooms found in --update-dir"
    )]
    NoOwner,
    #[error("the owner account {0} does not exist here")]
    UnknownOwner(String),
    #[error("these addresses already belong to another account: {}", .0.join(", "))]
    Conflicts(Vec<String>),
    #[error("cannot read the timestamp {text:?}: {reason}")]
    Timestamp { text: String, reason: String },
}

/// Why one room file was not migrated; the import goes on without it.
#[derive(Debug, Error)]
enum MigrateError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("a record is not an update: {0}")]
    Decode(#[from] yrs::encoding::read::Error),
    #[error("a record does not apply: {0}")]
    Apply(#[from] yrs::error::UpdateError),
    #[error(transparent)]
    Write(#[from] model::WriteError),
}

/// What an import did, or would do under `--dry-run`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub users: usize,
    pub rooms: usize,
    pub memberships: usize,
    pub invitations: usize,
    pub links: usize,
    /// Room files moved from layout v1 to v2.
    pub migrated: usize,
    pub skipped: Vec<String>,
}

impl Summary {
    pub fn lines(&self, dry_run: bool) -> Vec<String> {
        let verb = if dry_run { "would import" } else { "imported" };
        let mut lines = vec![format!(
            "{verb} {} users, {} rooms, {} memberships, {} invitations, {} linked accounts",
            self.users, self.rooms, self.memberships, self.invitations, self.links
        )];
        let migrated = if dry_run { "would migrate" } else { "migrated" };
        lines.push(format!(
            "{migrated} {} room files to layout v2",
            self.migrated
        ));
        for skipped in &self.skipped {
            lines.push(format!("skipped {skipped}"));
        }
        lines
    }
}

struct PythonUser {
    id: String,
    email: String,
    hashed_password: String,
    is_active: bool,
    is_superuser: bool,
    is_verified: bool,
    rooms: Option<String>,
}

pub async fn run(
    update_dir: &Path,
    db_path: &Path,
    users: Option<&Path>,
    owner: Option<&str>,
    dry_run: bool,
) -> Result<Summary, ImportError> {
    if let Some(users) = users
        && same_file(users, db_path)
    {
        return Err(ImportError::SameFile);
    }
    if users.is_none() && owner.is_none() {
        return Err(ImportError::NoOwner);
    }
    let pool = db::open(db_path).await?;
    let mut summary = Summary::default();
    if let Some(users) = users {
        import_users(&pool, users, dry_run, &mut summary).await?;
    }
    if let Some(owner) = owner {
        import_room_files(&pool, update_dir, owner, dry_run, &mut summary).await?;
    }
    migrate_room_files(update_dir, dry_run, &mut summary)?;
    Ok(summary)
}

fn migrate_room_files(
    update_dir: &Path,
    dry_run: bool,
    summary: &mut Summary,
) -> Result<(), ImportError> {
    for room_id in room_ids(update_dir)? {
        match migrate_room_file(update_dir, &room_id, dry_run) {
            Ok(true) => summary.migrated += 1,
            Ok(false) => {}
            Err(error) => summary.skipped.push(format!(
                "the room file {room_id}.y stays in layout v1: {error}"
            )),
        }
    }
    Ok(())
}

/// Appends the update that moves a v1 room file to v2; false when it is not v1.
fn migrate_room_file(
    update_dir: &Path,
    room_id: &str,
    dry_run: bool,
) -> Result<bool, MigrateError> {
    let (mut store, updates) = FileRoomStore::open(update_dir, room_id)?;
    let doc = Doc::new();
    {
        let mut txn = doc.transact_mut();
        for update in &updates {
            txn.apply_update(Update::decode_v1(update)?)?;
        }
    }
    let Some(update) = model::migrate_v1(&doc)? else {
        return Ok(false);
    };
    if !dry_run {
        store.append(Bytes::from(update))?;
        store.flush()?;
    }
    Ok(true)
}

/// The ids of the room files in `update_dir`, sorted.
fn room_ids(update_dir: &Path) -> Result<BTreeSet<String>, ImportError> {
    let io = |source| ImportError::Io {
        path: update_dir.to_path_buf(),
        source,
    };
    let mut ids = BTreeSet::new();
    for entry in std::fs::read_dir(update_dir).map_err(io)? {
        if let Some(room_id) = cocat_core::store::room_id_from_file(&entry.map_err(io)?.path()) {
            ids.insert(room_id);
        }
    }
    Ok(ids)
}

async fn import_users(
    pool: &SqlitePool,
    path: &Path,
    dry_run: bool,
    summary: &mut Summary,
) -> Result<(), ImportError> {
    let python = open_python(path).await?;
    let released = !has_table(&python, "membership")
        .await
        .map_err(|error| python_error(path, error))?;
    let listed = has_column(&python, "user", "rooms")
        .await
        .map_err(|error| python_error(path, error))?;
    let select = if listed {
        "SELECT id, email, hashed_password, is_active, is_superuser, is_verified, rooms FROM \"user\""
    } else {
        "SELECT id, email, hashed_password, is_active, is_superuser, is_verified FROM \"user\""
    };
    let rows = fetch(&python, select)
        .await
        .map_err(|error| python_error(path, error))?;
    let users: Vec<PythonUser> = rows
        .iter()
        .map(|row| PythonUser {
            id: row.get("id"),
            email: row.get("email"),
            hashed_password: row.get("hashed_password"),
            is_active: row.get("is_active"),
            is_superuser: row.get("is_superuser"),
            is_verified: row.get("is_verified"),
            rooms: listed.then(|| row.get("rooms")).flatten(),
        })
        .collect();

    refuse_conflicts(pool, &users).await?;

    let emails: BTreeMap<String, String> = users
        .iter()
        .map(|user| (user.id.clone(), user.email.clone()))
        .collect();
    let mut members: Vec<(String, String)> = Vec::new();
    let mut tx = db::begin_immediate(pool).await?;
    for user in &users {
        sqlx::query(
            "INSERT OR IGNORE INTO \"user\" (id, email, hashed_password, is_active, is_verified, is_superuser) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&user.id)
        .bind(&user.email)
        .bind(&user.hashed_password)
        .bind(user.is_active)
        .bind(if released { true } else { user.is_verified })
        .bind(user.is_superuser)
        .execute(&mut *tx)
        .await?;
        summary.users += 1;
        if !released {
            continue;
        }
        for room_id in listed_rooms(user, summary) {
            sqlx::query("INSERT OR IGNORE INTO room (id, name) VALUES (?, ?)")
                .bind(&room_id)
                .bind(&room_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT OR IGNORE INTO membership (room_id, user_id, role) VALUES (?, ?, 'editor')",
            )
            .bind(&room_id)
            .bind(&user.id)
            .execute(&mut *tx)
            .await?;
            members.push((room_id.clone(), user.id.clone()));
            summary.rooms += 1;
            summary.memberships += 1;
        }
    }

    if released {
        pick_owners(&mut tx, &members, &emails).await?;
    } else {
        copy_rooms(&mut tx, &python, summary).await?;
    }

    if dry_run {
        tx.rollback().await.map_err(db::DbError::from)?;
    } else {
        tx.commit().await.map_err(db::DbError::from)?;
    }
    Ok(())
}

/// Python's `_pick_owner`: the first member by email whose local part names the room, else
/// the first member by email.
async fn pick_owners(
    tx: &mut sqlx::Transaction<'static, sqlx::Sqlite>,
    members: &[(String, String)],
    emails: &BTreeMap<String, String>,
) -> Result<(), db::DbError> {
    let mut by_room: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
    for (room, user) in members {
        let email = emails.get(user).map(String::as_str).unwrap_or_default();
        by_room
            .entry(room.as_str())
            .or_default()
            .push((user.as_str(), email));
    }
    for (room, mut candidates) in by_room {
        candidates.sort_by(|left, right| left.1.cmp(right.1));
        let local = room;
        let owner = candidates
            .iter()
            .find(|(_, email)| email.split('@').next() == Some(local))
            .copied()
            .unwrap_or(candidates[0]);
        sqlx::query("UPDATE membership SET role = 'owner' WHERE room_id = ? AND user_id = ?")
            .bind(room)
            .bind(owner.0)
            .execute(&mut **tx)
            .await?;
        sqlx::query("UPDATE room SET created_by = ? WHERE id = ?")
            .bind(owner.0)
            .bind(room)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

fn listed_rooms(user: &PythonUser, summary: &mut Summary) -> Vec<String> {
    let Some(rooms) = &user.rooms else {
        summary
            .skipped
            .push(format!("the rooms of {}: no JSON column", user.email));
        return Vec::new();
    };
    match serde_json::from_str::<serde_json::Value>(rooms) {
        Ok(serde_json::Value::Array(items)) => {
            let mut ids = Vec::new();
            for item in items {
                match item.as_str() {
                    Some(id) => ids.push(id.to_owned()),
                    None => {
                        summary.skipped.push(format!(
                            "the rooms of {}: not a JSON list of strings",
                            user.email
                        ));
                        return Vec::new();
                    }
                }
            }
            ids
        }
        _ => {
            summary.skipped.push(format!(
                "the rooms of {}: not a JSON list of strings",
                user.email
            ));
            Vec::new()
        }
    }
}

async fn copy_rooms(
    tx: &mut sqlx::Transaction<'static, sqlx::Sqlite>,
    python: &SqlitePool,
    summary: &mut Summary,
) -> Result<(), ImportError> {
    for row in fetch(python, "SELECT id, name, created_at, created_by FROM room")
        .await
        .map_err(|error| python_error(Path::new(""), error))?
    {
        sqlx::query(
            "INSERT OR IGNORE INTO room (id, name, created_at, created_by) VALUES (?, ?, ?, ?)",
        )
        .bind(row.get::<String, _>("id"))
        .bind(row.get::<String, _>("name"))
        .bind(space_time(&row.get::<String, _>("created_at"))?)
        .bind(row.get::<Option<String>, _>("created_by"))
        .execute(&mut **tx)
        .await?;
        summary.rooms += 1;
    }
    for row in fetch(python, "SELECT room_id, user_id, role FROM membership")
        .await
        .map_err(|error| python_error(Path::new(""), error))?
    {
        sqlx::query("INSERT OR IGNORE INTO membership (room_id, user_id, role) VALUES (?, ?, ?)")
            .bind(row.get::<String, _>("room_id"))
            .bind(row.get::<String, _>("user_id"))
            .bind(row.get::<String, _>("role"))
            .execute(&mut **tx)
            .await?;
        summary.memberships += 1;
    }
    for row in fetch(
        python,
        "SELECT token_hash, room_id, role, email, created_by, expires_at, revoked, max_uses, uses FROM invitation",
    )
    .await
    .map_err(|error| python_error(Path::new(""), error))?
    {
        sqlx::query(
            "INSERT OR IGNORE INTO invitation (token_hash, room_id, role, email, created_by, expires_at, revoked, max_uses, uses) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(row.get::<String, _>("token_hash"))
        .bind(row.get::<String, _>("room_id"))
        .bind(row.get::<String, _>("role"))
        .bind(row.get::<Option<String>, _>("email"))
        .bind(row.get::<Option<String>, _>("created_by"))
        .bind(instant(&row.get::<String, _>("expires_at"))?)
        .bind(row.get::<bool, _>("revoked"))
        .bind(row.get::<Option<i64>, _>("max_uses"))
        .bind(row.get::<i64, _>("uses"))
        .execute(&mut **tx)
        .await?;
        summary.invitations += 1;
    }
    for row in fetch(
        python,
        "SELECT user_id, oauth_name, account_id FROM oauth_account",
    )
    .await
    .map_err(|error| python_error(Path::new(""), error))?
    {
        sqlx::query(
            "INSERT OR IGNORE INTO oauth_account (provider, subject, user_id) VALUES (?, ?, ?)",
        )
        .bind(row.get::<String, _>("oauth_name"))
        .bind(row.get::<String, _>("account_id"))
        .bind(row.get::<String, _>("user_id"))
        .execute(&mut **tx)
        .await?;
        summary.links += 1;
    }
    Ok(())
}

async fn import_room_files(
    pool: &SqlitePool,
    update_dir: &Path,
    owner: &str,
    dry_run: bool,
    summary: &mut Summary,
) -> Result<(), ImportError> {
    let owner_id = match db::user_by_email(pool, owner).await? {
        Some(user) => user.id,
        None => return Err(ImportError::UnknownOwner(owner.to_owned())),
    };
    let mut ids: BTreeSet<String> = BTreeSet::new();
    for room_id in room_ids(update_dir)? {
        if let Err(error) = FileRoomStore::open(update_dir, &room_id) {
            summary
                .skipped
                .push(format!("the room file {room_id}.y: {error}"));
            continue;
        }
        ids.insert(room_id);
    }
    let mut tx = db::begin_immediate(pool).await?;
    for room_id in ids {
        let known: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM room WHERE id = ?")
            .bind(&room_id)
            .fetch_one(&mut *tx)
            .await?;
        if known > 0 {
            continue;
        }
        sqlx::query("INSERT INTO room (id, name, created_by) VALUES (?, ?, ?)")
            .bind(&room_id)
            .bind(&room_id)
            .bind(owner_id.to_string())
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO membership (room_id, user_id, role) VALUES (?, ?, 'owner')")
            .bind(&room_id)
            .bind(owner_id.to_string())
            .execute(&mut *tx)
            .await?;
        summary.rooms += 1;
        summary.memberships += 1;
    }
    if dry_run {
        tx.rollback().await.map_err(db::DbError::from)?;
    } else {
        tx.commit().await.map_err(db::DbError::from)?;
    }
    Ok(())
}

async fn refuse_conflicts(pool: &SqlitePool, users: &[PythonUser]) -> Result<(), ImportError> {
    let mut conflicts = Vec::new();
    for user in users {
        if let Some(existing) = db::user_by_email(pool, &user.email).await?
            && existing.id.to_string() != user.id
        {
            conflicts.push(user.email.clone());
        }
    }
    if conflicts.is_empty() {
        Ok(())
    } else {
        Err(ImportError::Conflicts(conflicts))
    }
}

async fn open_python(path: &Path) -> Result<SqlitePool, ImportError> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .create_if_missing(false);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|source| python_error(path, source))
}

async fn fetch(
    pool: &SqlitePool,
    sql: &'static str,
) -> Result<Vec<sqlx::sqlite::SqliteRow>, sqlx::Error> {
    sqlx::query(sql).fetch_all(pool).await
}

async fn has_table(pool: &SqlitePool, name: &str) -> Result<bool, sqlx::Error> {
    let found: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?")
            .bind(name)
            .fetch_one(pool)
            .await?;
    Ok(found > 0)
}

async fn has_column(pool: &SqlitePool, table: &str, column: &str) -> Result<bool, sqlx::Error> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("PRAGMA table_info({table})")))
        .fetch_all(pool)
        .await?;
    Ok(rows
        .iter()
        .any(|row| row.get::<String, _>("name") == column))
}

fn same_file(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn python_error(path: &Path, source: sqlx::Error) -> ImportError {
    ImportError::Python {
        path: path.to_path_buf(),
        source,
    }
}

/// Python writes naive UTC timestamps as `YYYY-MM-DD HH:MM:SS.ffffff`; every copied value is
/// parsed here and written in the form its Rust column already holds.
fn python_time(text: &str) -> Result<OffsetDateTime, ImportError> {
    const WITH_FRACTION: &[BorrowedFormatItem<'static>] =
        format_description!("[year]-[month]-[day] [hour]:[minute]:[second].[subsecond]");
    const PLAIN: &[BorrowedFormatItem<'static>] =
        format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
    let parsed = PrimitiveDateTime::parse(text, WITH_FRACTION)
        .or_else(|_| PrimitiveDateTime::parse(text, PLAIN))
        .map_err(|error| ImportError::Timestamp {
            text: text.to_owned(),
            reason: error.to_string(),
        })?;
    Ok(parsed.assume_utc())
}

/// The space-separated form SQLite's `CURRENT_TIMESTAMP` writes for `created_at` and `joined_at`.
fn space_time(text: &str) -> Result<String, ImportError> {
    const SPACE: &[BorrowedFormatItem<'static>] =
        format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
    python_time(text)?
        .format(SPACE)
        .map_err(|error| ImportError::Timestamp {
            text: text.to_owned(),
            reason: error.to_string(),
        })
}

/// The form `strftime('%Y-%m-%dT%H:%M:%fZ')` writes, which invitation comparisons use.
fn instant(text: &str) -> Result<String, ImportError> {
    const INSTANT: &[BorrowedFormatItem<'static>] =
        format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");
    python_time(text)?
        .format(INSTANT)
        .map_err(|error| ImportError::Timestamp {
            text: text.to_owned(),
            reason: error.to_string(),
        })
}
