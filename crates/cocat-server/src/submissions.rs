//! The stored submissions to the public catalogues, and the links from a copied catalogue
//! back to the public one it proposes a change to.

use sqlx::{AssertSqlSafe, SqlitePool};
use uuid::Uuid;

use crate::db::DbError;

/// The columns every listing reads, emails resolved; the snapshot is read on its own. Only
/// these constants are spliced into the queries, hence `AssertSqlSafe`.
const COLUMNS: &str = "submission.id, submission.author, author.email AS author_email,
    submission.kind, submission.target, submission.base, submission.catalogue,
    submission.title, submission.folder, submission.description, submission.note,
    submission.status, reviewer.email AS reviewer_email, submission.review_note,
    submission.created_at, submission.decided_at";
const FROM: &str = r#"FROM submission JOIN "user" AS author ON author.id = submission.author
    LEFT JOIN "user" AS reviewer ON reviewer.id = submission.reviewer"#;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Submission {
    pub id: i64,
    pub author: String,
    pub author_email: String,
    pub kind: String,
    pub target: Option<String>,
    pub base: Option<i64>,
    pub catalogue: String,
    pub title: String,
    pub folder: Option<String>,
    pub description: Option<String>,
    pub note: Option<String>,
    pub status: String,
    pub reviewer_email: Option<String>,
    pub review_note: Option<String>,
    pub created_at: String,
    pub decided_at: Option<String>,
}

impl Submission {
    pub fn is_by(&self, user: Uuid) -> bool {
        self.author == user.to_string()
    }

    pub fn is_pending(&self) -> bool {
        self.status == PENDING
    }
}

pub const PENDING: &str = "pending";
pub const ACCEPTED: &str = "accepted";
pub const DECLINED: &str = "declined";
pub const WITHDRAWN: &str = "withdrawn";

/// What an author offers, from project `room`. `target` and `base` are set for an update.
pub struct NewSubmission<'a> {
    pub author: Uuid,
    pub room: &'a str,
    pub kind: &'a str,
    pub target: Option<&'a str>,
    pub base: Option<i64>,
    pub catalogue: &'a str,
    pub title: &'a str,
    pub folder: Option<&'a str>,
    pub description: Option<&'a str>,
    pub note: Option<&'a str>,
    pub snapshot: &'a [u8],
}

pub async fn insert(pool: &SqlitePool, new: &NewSubmission<'_>) -> Result<i64, DbError> {
    Ok(sqlx::query_scalar(
        "INSERT INTO submission
             (author, room, kind, target, base, catalogue, title, folder, description, note,
              snapshot)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(new.author.to_string())
    .bind(new.room)
    .bind(new.kind)
    .bind(new.target)
    .bind(new.base)
    .bind(new.catalogue)
    .bind(new.title)
    .bind(new.folder)
    .bind(new.description)
    .bind(new.note)
    .bind(new.snapshot)
    .fetch_one(pool)
    .await?)
}

pub async fn one(pool: &SqlitePool, id: i64) -> Result<Option<Submission>, DbError> {
    Ok(sqlx::query_as(AssertSqlSafe(format!(
        "SELECT {COLUMNS} {FROM} WHERE submission.id = ?"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await?)
}

/// The snapshot: the catalogue and its events as one Yjs update.
pub async fn snapshot(pool: &SqlitePool, id: i64) -> Result<Vec<u8>, DbError> {
    Ok(
        sqlx::query_scalar("SELECT snapshot FROM submission WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await?,
    )
}

/// How many of `author`'s submissions wait for a curator.
pub async fn pending(pool: &SqlitePool, author: Uuid) -> Result<i64, DbError> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM submission WHERE author = ? AND status = 'pending'",
    )
    .bind(author.to_string())
    .fetch_one(pool)
    .await?)
}

/// The bytes `author`'s stored snapshots take, decided ones included: they stay on disk.
pub async fn stored_bytes(pool: &SqlitePool, author: Uuid) -> Result<u64, DbError> {
    let bytes: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(length(snapshot)), 0) FROM submission WHERE author = ?",
    )
    .bind(author.to_string())
    .fetch_one(pool)
    .await?;
    Ok(u64::try_from(bytes).unwrap_or(0))
}

/// Every submission, or only `author`'s: pending ones first, then the newest.
pub async fn list(pool: &SqlitePool, author: Option<Uuid>) -> Result<Vec<Submission>, DbError> {
    Ok(sqlx::query_as(AssertSqlSafe(format!(
        "SELECT {COLUMNS} {FROM}
         WHERE ?1 IS NULL OR submission.author = ?1
         ORDER BY submission.status != 'pending', submission.id DESC"
    )))
    .bind(author.map(|author| author.to_string()))
    .fetch_all(pool)
    .await?)
}

/// What waits for a curator, oldest first.
pub async fn waiting(pool: &SqlitePool) -> Result<Vec<Submission>, DbError> {
    Ok(sqlx::query_as(AssertSqlSafe(format!(
        "SELECT {COLUMNS} {FROM} WHERE submission.status = 'pending' ORDER BY submission.id"
    )))
    .fetch_all(pool)
    .await?)
}

/// `author`'s submissions still pending, and those accepted or declined in the last 30
/// days: pending first, then the newest.
pub async fn recent(pool: &SqlitePool, author: Uuid) -> Result<Vec<Submission>, DbError> {
    Ok(sqlx::query_as(AssertSqlSafe(format!(
        "SELECT {COLUMNS} {FROM}
         WHERE submission.author = ?
           AND (submission.status = 'pending'
                OR (submission.status IN ('accepted', 'declined')
                    AND submission.decided_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-30 days')))
         ORDER BY submission.status != 'pending', submission.id DESC"
    )))
    .bind(author.to_string())
    .fetch_all(pool)
    .await?)
}

/// A curator's decision, or an author's withdrawal: only a pending submission takes one.
pub struct Decision<'a> {
    pub status: &'static str,
    pub by: Uuid,
    pub note: Option<&'a str>,
}

/// Records the decision; false when the submission was no longer pending.
pub async fn decide(pool: &SqlitePool, id: i64, decision: &Decision<'_>) -> Result<bool, DbError> {
    let reviewer = (decision.status != WITHDRAWN).then(|| decision.by.to_string());
    let updated = sqlx::query(
        "UPDATE submission SET status = ?, reviewer = ?, review_note = ?,
             decided_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
         WHERE id = ? AND status = 'pending'",
    )
    .bind(decision.status)
    .bind(reviewer)
    .bind(decision.note)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(updated.rows_affected() == 1)
}

/// Puts an accepted submission back to pending, when taking it in failed.
pub async fn reopen(pool: &SqlitePool, id: i64) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE submission SET status = 'pending', reviewer = NULL, review_note = NULL,
             decided_at = NULL
         WHERE id = ?",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// The public catalogue an accepted new submission became.
pub async fn set_target(pool: &SqlitePool, id: i64, target: &str) -> Result<(), DbError> {
    sqlx::query("UPDATE submission SET target = ? WHERE id = ?")
        .bind(target)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// A catalogue of `room_id` copied out of the public catalogue `target`.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Proposal {
    pub catalogue: String,
    pub target: String,
    pub base: i64,
    /// The public catalogue's name.
    pub name: String,
}

pub async fn link(
    pool: &SqlitePool,
    room_id: &str,
    catalogue: &str,
    target: &str,
    base: i64,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO proposal (room_id, catalogue, target, base) VALUES (?, ?, ?, ?)
         ON CONFLICT (room_id, catalogue) DO UPDATE SET target = excluded.target,
             base = excluded.base",
    )
    .bind(room_id)
    .bind(catalogue)
    .bind(target)
    .bind(base)
    .execute(pool)
    .await?;
    Ok(())
}

/// After an update was accepted, the copy it came from holds what the public catalogue
/// holds once it had `base` changes.
pub async fn rebase(pool: &SqlitePool, id: i64, base: i64) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE proposal SET base = ?1
         WHERE (room_id, catalogue, target) =
             (SELECT room, catalogue, target FROM submission WHERE id = ?2)",
    )
    .bind(base)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// The catalogues of a room copied out of public catalogues, by catalogue.
pub async fn proposals(pool: &SqlitePool, room_id: &str) -> Result<Vec<Proposal>, DbError> {
    Ok(sqlx::query_as(
        "SELECT proposal.catalogue, proposal.target, proposal.base, room.name
         FROM proposal JOIN room ON room.id = proposal.target
         WHERE proposal.room_id = ? ORDER BY proposal.catalogue",
    )
    .bind(room_id)
    .fetch_all(pool)
    .await?)
}
