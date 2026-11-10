//! What the relay keeps in the database: each provider's settings and last run, and which
//! public catalogues follow which provider.

use sqlx::SqlitePool;

use crate::db::DbError;

/// A provider's settings and how its last run went.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ProviderRow {
    pub id: String,
    pub enabled: bool,
    /// A cron line; the provider's default when `None`.
    pub schedule: Option<String>,
    /// A JSON object the provider reads.
    pub settings: String,
    pub running: bool,
    pub last_run_at: Option<String>,
    pub last_ok_at: Option<String>,
    /// A JSON object: the counts of a run that worked, or `{"error": …}`.
    pub last_result: Option<String>,
}

/// A public catalogue that follows a provider.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct RelayedRow {
    pub provider: String,
    pub key: String,
    /// The provider's source name, as the room records it.
    pub source: String,
    /// False once the provider stopped publishing it: it stays, no longer refreshed.
    pub published: bool,
    pub refreshed_at: Option<String>,
    pub url: Option<String>,
}

impl RelayedRow {
    /// Why nobody edits it by hand.
    pub fn refusal(&self) -> String {
        let source = &self.source;
        format!("This catalogue follows {source}: it changes when {source} does.")
    }
}

pub async fn provider(pool: &SqlitePool, id: &str) -> Result<Option<ProviderRow>, DbError> {
    Ok(sqlx::query_as(
        "SELECT id, enabled, schedule, settings, running, last_run_at, last_ok_at, last_result
         FROM relay_provider WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?)
}

/// Marks the provider running; false when it already was. The update itself tells, so two
/// callers racing cannot both start it.
pub async fn start(pool: &SqlitePool, id: &str) -> Result<bool, DbError> {
    sqlx::query("INSERT OR IGNORE INTO relay_provider (id) VALUES (?)")
        .bind(id)
        .execute(pool)
        .await?;
    let started = sqlx::query("UPDATE relay_provider SET running = 1 WHERE id = ? AND NOT running")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(started.rows_affected() == 1)
}

/// Records the end of a run: when, whether it worked, and what it did or why it failed.
pub async fn finish(pool: &SqlitePool, id: &str, ok: bool, result: &str) -> Result<(), DbError> {
    // One statement reads one 'now', so a run that worked has equal times.
    sqlx::query(
        "UPDATE relay_provider SET running = 0,
           last_run_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
           last_ok_at = CASE WHEN ? THEN strftime('%Y-%m-%dT%H:%M:%SZ', 'now') ELSE last_ok_at END,
           last_result = ?
         WHERE id = ?",
    )
    .bind(ok)
    .bind(result)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// No run survives a restart: whatever was marked running stopped with the old process.
pub async fn stop_all(pool: &SqlitePool) -> Result<(), DbError> {
    sqlx::query("UPDATE relay_provider SET running = 0")
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn settings(pool: &SqlitePool, id: &str) -> Result<String, DbError> {
    let settings: Option<String> =
        sqlx::query_scalar("SELECT settings FROM relay_provider WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(settings.unwrap_or_else(|| "{}".to_owned()))
}

/// Makes the public catalogue follow `provider`'s list `key`, published.
pub async fn adopt(
    pool: &SqlitePool,
    room_id: &str,
    provider: &str,
    key: &str,
    url: &str,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO relay_catalogue (room_id, provider, key, url) VALUES (?, ?, ?, ?)
         ON CONFLICT (room_id) DO UPDATE SET provider = excluded.provider, key = excluded.key,
           url = excluded.url, published = 1",
    )
    .bind(room_id)
    .bind(provider)
    .bind(key)
    .bind(url)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn refreshed(pool: &SqlitePool, room_id: &str) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE relay_catalogue SET refreshed_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
         WHERE room_id = ?",
    )
    .bind(room_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Marks the provider's catalogues outside `published` as no longer published; how many of
/// its catalogues are so now.
pub async fn unpublish_others(
    pool: &SqlitePool,
    provider: &str,
    published: &[String],
) -> Result<usize, DbError> {
    let published = serde_json::Value::from(published).to_string();
    sqlx::query(
        "UPDATE relay_catalogue SET published = 0
         WHERE provider = ? AND room_id NOT IN (SELECT value FROM json_each(?))",
    )
    .bind(provider)
    .bind(published)
    .execute(pool)
    .await?;
    let unpublished: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM relay_catalogue WHERE provider = ? AND NOT published",
    )
    .bind(provider)
    .fetch_one(pool)
    .await?;
    Ok(usize::try_from(unpublished).unwrap_or(0))
}

pub async fn relayed(pool: &SqlitePool, room_id: &str) -> Result<Option<RelayedRow>, DbError> {
    Ok(sqlx::query_as(
        "SELECT relay_catalogue.provider, relay_catalogue.key,
           COALESCE(room.source, relay_catalogue.provider) AS source,
           relay_catalogue.published, relay_catalogue.refreshed_at, relay_catalogue.url
         FROM relay_catalogue JOIN room ON room.id = relay_catalogue.room_id
         WHERE relay_catalogue.room_id = ?",
    )
    .bind(room_id)
    .fetch_optional(pool)
    .await?)
}
