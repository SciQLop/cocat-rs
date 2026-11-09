//! Browser sign-in for apps, device-style: the app asks for a request and shows its link and
//! code; the person opens the link, signs in, checks the code and allows or denies it; the app
//! polls and, once allowed, receives a token, once.
//!
//! Polling rather than a local listener: the browser and the app need not share a machine,
//! which is the case on JupyterHub or over SSH.

use data_encoding::{Encoding, Specification, SpecificationError};
use sqlx::{Row, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

use crate::auth::{self, AuthError};
use crate::db::DbError;

pub const LIFETIME_SECONDS: u32 = 600;
pub const POLL_INTERVAL_SECONDS: u32 = 2;
pub const MAX_NAME_CHARS: usize = 100;

/// Crockford's base32: digits and capitals without I, L, O and U, so a code read aloud or
/// copied by hand cannot be misread.
const CODE_SYMBOLS: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// Five bytes are eight symbols, shown as two groups of four.
const CODE_BYTES: usize = 5;

#[derive(Debug, Error)]
pub enum SignInError {
    #[error("the request has no name")]
    NoName,
    #[error("cannot draw a code: {0}")]
    Random(#[from] getrandom::Error),
    #[error("the code alphabet is invalid: {0}")]
    Alphabet(#[from] SpecificationError),
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<sqlx::Error> for SignInError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(DbError::from(error))
    }
}

/// What the app gets back: the code to show and the secret to poll with.
#[derive(Debug)]
pub struct Created {
    pub code: String,
    pub poll: String,
}

/// What a poll finds.
#[derive(Debug, PartialEq, Eq)]
pub enum Polled {
    Pending,
    Denied,
    /// Unknown, expired, or already handed out: the app starts over in every case.
    Expired,
    /// Polled again within the interval; nothing changed.
    SlowDown,
    Approved {
        token: String,
    },
}

/// A request still waiting for the person, as the page shows it.
#[derive(Debug)]
pub struct Waiting {
    pub code: String,
    pub name: String,
}

/// Open a request for the app called `name`, clearing the ones whose time is up.
pub async fn create(pool: &SqlitePool, name: &str) -> Result<Created, SignInError> {
    let name: String = name.trim().chars().take(MAX_NAME_CHARS).collect();
    if name.is_empty() {
        return Err(SignInError::NoName);
    }
    sqlx::query(
        "DELETE FROM sign_in_request WHERE expires_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
    )
    .execute(pool)
    .await?;
    let code = new_code()?;
    let (poll, poll_hash) = crate::invitations::new_token()?;
    sqlx::query(
        "INSERT INTO sign_in_request (poll_hash, code, name, expires_at)
         VALUES (?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?))",
    )
    .bind(&poll_hash)
    .bind(&code)
    .bind(&name)
    .bind(format!("+{LIFETIME_SECONDS} seconds"))
    .execute(pool)
    .await?;
    Ok(Created { code, poll })
}

/// Answer a poll. An approved request turns into a token and disappears in one statement, so
/// two polls racing for it cannot both receive one.
pub async fn poll(pool: &SqlitePool, poll: &str) -> Result<Polled, SignInError> {
    let poll_hash = crate::invitations::hash(poll);
    if let Some((user_id, name)) = take_approved(pool, &poll_hash).await? {
        let token = auth::start_token(pool, user_id, &name).await?;
        return Ok(Polled::Approved { token });
    }
    let Some(row) = sqlx::query(
        "SELECT status,
                expires_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now') AS expired,
                polled_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 seconds') AS too_soon
         FROM sign_in_request WHERE poll_hash = ?",
    )
    .bind(&poll_hash)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(Polled::Expired);
    };
    let status: String = row.try_get("status")?;
    let expired: bool = row.try_get("expired")?;
    let too_soon: Option<bool> = row.try_get("too_soon")?;
    if expired || status == "denied" {
        delete(pool, &poll_hash).await?;
        return Ok(if expired {
            Polled::Expired
        } else {
            Polled::Denied
        });
    }
    if too_soon == Some(true) {
        return Ok(Polled::SlowDown);
    }
    sqlx::query(
        "UPDATE sign_in_request SET polled_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE poll_hash = ?",
    )
    .bind(&poll_hash)
    .execute(pool)
    .await?;
    Ok(Polled::Pending)
}

/// The request a code names, while it still waits for an answer.
pub async fn waiting(pool: &SqlitePool, code: &str) -> Result<Option<Waiting>, SignInError> {
    let row = sqlx::query(
        "SELECT code, name FROM sign_in_request
         WHERE code = ? AND status = 'pending'
           AND expires_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
    )
    .bind(normalize(code))
    .fetch_optional(pool)
    .await?;
    let waiting = row.map(|row| {
        Ok::<_, sqlx::Error>(Waiting {
            code: row.try_get("code")?,
            name: row.try_get("name")?,
        })
    });
    Ok(waiting.transpose()?)
}

/// Record the person's answer; `None` when the request is gone or was already answered.
pub async fn decide(
    pool: &SqlitePool,
    code: &str,
    user_id: Uuid,
    allow: bool,
) -> Result<Option<String>, SignInError> {
    let row = sqlx::query(
        "UPDATE sign_in_request SET status = ?, user_id = ?
         WHERE code = ? AND status = 'pending'
           AND expires_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         RETURNING name",
    )
    .bind(if allow { "approved" } else { "denied" })
    .bind(user_id.to_string())
    .bind(normalize(code))
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| row.try_get("name")).transpose()?)
}

async fn take_approved(
    pool: &SqlitePool,
    poll_hash: &str,
) -> Result<Option<(Uuid, String)>, SignInError> {
    let row = sqlx::query(
        "DELETE FROM sign_in_request
         WHERE poll_hash = ? AND status = 'approved'
           AND expires_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         RETURNING user_id, name",
    )
    .bind(poll_hash)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let user_id: String = row.try_get("user_id")?;
    let user_id =
        Uuid::parse_str(&user_id).map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
    Ok(Some((user_id, row.try_get("name")?)))
}

async fn delete(pool: &SqlitePool, poll_hash: &str) -> Result<(), SignInError> {
    sqlx::query("DELETE FROM sign_in_request WHERE poll_hash = ?")
        .bind(poll_hash)
        .execute(pool)
        .await?;
    Ok(())
}

fn new_code() -> Result<String, SignInError> {
    let mut bytes = [0u8; CODE_BYTES];
    getrandom::fill(&mut bytes)?;
    let symbols = code_encoding()?.encode(&bytes);
    let (first, second) = symbols.split_at(symbols.len() / 2);
    Ok(format!("{first}-{second}"))
}

fn code_encoding() -> Result<Encoding, SpecificationError> {
    let mut specification = Specification::new();
    specification.symbols.push_str(CODE_SYMBOLS);
    specification.encoding()
}

/// A code as typed or pasted: any case, stray spaces.
fn normalize(code: &str) -> String {
    code.trim().to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_is_two_groups_of_four_unambiguous_symbols() {
        let code = new_code().unwrap();
        let (first, second) = code.split_once('-').unwrap();
        assert_eq!((first.len(), second.len()), (4, 4), "{code}");
        assert!(
            code.chars()
                .filter(|c| *c != '-')
                .all(|c| CODE_SYMBOLS.contains(c)),
            "{code}"
        );
    }

    #[test]
    fn a_typed_code_is_read_in_any_case() {
        assert_eq!(normalize(" abcd-1234 "), "ABCD-1234");
    }
}
