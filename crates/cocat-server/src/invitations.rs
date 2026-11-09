//! Invitations: a link that adds its bearer to a room. The rules are Python's
//! `invitations.py`; the conditional `UPDATE` is what makes `max_uses` hold under a race.

use std::str::FromStr;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use cocat_core::role::Role;
use sha2::{Digest, Sha256};
use sqlx::sqlite::SqliteRow;
use sqlx::{AssertSqlSafe, Row};
use uuid::Uuid;

use crate::AppState;
use crate::db::{self, RoomRow};
use crate::lifecycle::{LifecycleError, require_sharer};
use crate::mail::{self, Mailer};
use crate::room::CloseReason;

/// One predicate, used by preview, accept, consume and list: an invitation nobody revoked,
/// that has not expired, and that still has a use left. It and the constant query heads
/// below are the only text these queries build dynamically, hence the `AssertSqlSafe` around
/// each `format!`. Group invitations use
/// the same rule over their own table.
pub(crate) fn usable(table: &str) -> String {
    format!(
        "{table}.revoked = FALSE
     AND {table}.expires_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
     AND ({table}.max_uses IS NULL OR {table}.uses < {table}.max_uses)"
    )
}

const TOKEN_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invitation {
    pub id: i64,
    pub email: Option<String>,
    pub role: Role,
    pub expires_at: String,
    pub uses: i64,
    pub max_uses: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub room_id: String,
    pub room_name: String,
    pub role: Role,
    pub inviter_email: Option<String>,
    pub expires_at: String,
}

/// Creates an invitation and returns its id and its token, the only time the token exists
/// in clear — unless the invitation was mailed, in which case the token comes back empty so a
/// reply cannot carry a second copy of a link that travelled to its addressee. An addressed
/// invitation admits one use, as Python's does.
pub async fn create(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
    role: Role,
    email: Option<&str>,
    expires_in_days: u32,
    max_uses: Option<u32>,
) -> Result<(i64, String), LifecycleError> {
    require_sharer(state, room_id, actor).await?;
    if role == Role::Owner {
        return Err(LifecycleError::Conflict(
            "An invitation cannot grant the owner role",
        ));
    }
    let (token, hash) = new_token()?;
    let max_uses = if email.is_some() { Some(1) } else { max_uses };
    let id = sqlx::query(
        "INSERT INTO invitation
             (token_hash, room_id, role, email, created_by, expires_at, max_uses)
         VALUES (?, ?, ?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?), ?)",
    )
    .bind(hash)
    .bind(room_id)
    .bind(role.as_str())
    .bind(email)
    .bind(actor.to_string())
    .bind(format!("+{expires_in_days} days"))
    .bind(max_uses.map(i64::from))
    .execute(&state.pool)
    .await?
    .last_insert_rowid();
    if let (Some(mailer), Some(address)) = (&state.mailer, email) {
        mail_invitation(state, mailer, actor, room_id, id, &token, address).await?;
        return Ok((id, String::new()));
    }
    Ok((id, token))
}

/// Mail the link to the address the invitation names, and undo the invitation if the relay
/// refuses: an addressed invitation nobody received cannot be used and must not sit in the
/// list. The token never reaches the log, so only the failure is recorded.
async fn mail_invitation(
    state: &AppState,
    mailer: &Mailer,
    actor: Uuid,
    room_id: &str,
    id: i64,
    token: &str,
    address: &str,
) -> Result<(), LifecycleError> {
    let row = sqlx::query(
        "SELECT room.name, \"user\".email
         FROM room JOIN \"user\" ON \"user\".id = ?
         WHERE room.id = ?",
    )
    .bind(actor.to_string())
    .bind(room_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| LifecycleError::NotFound(format!("Project {room_id} not found")))?;
    let room_name: String = row.try_get("name")?;
    let inviter: String = row.try_get("email")?;
    let (subject, body) = mail::invitation_mail(
        &room_name,
        &inviter,
        &state.links.ui(&format!("/invitation#{token}")),
    );
    if let Err(error) = mailer.send(address, &subject, &body).await {
        tracing::warn!(%error, "could not mail the invitation");
        sqlx::query("DELETE FROM invitation WHERE id = ?")
            .bind(id)
            .execute(&state.pool)
            .await?;
        return Err(LifecycleError::Invalid(
            "The email could not be sent; no invitation was created",
        ));
    }
    Ok(())
}

/// The addresses an invitation may suggest: the people `user` works with, minus `members`.
pub(crate) async fn suggestions(
    state: &AppState,
    user: Uuid,
    members: &[&str],
) -> Result<Vec<String>, LifecycleError> {
    let mut people = db::known_people(&state.pool, user).await?;
    people.retain(|email| !members.contains(&email.as_str()));
    Ok(people)
}

/// What a guest sees of an invitation: the predicate follows in each query.
const PREVIEW: &str = r#"SELECT invitation.id, invitation.room_id, invitation.role,
        invitation.expires_at, room.name, "user".email AS inviter_email
    FROM invitation
    JOIN room ON room.id = invitation.room_id
    LEFT JOIN "user" ON "user".id = invitation.created_by"#;

pub async fn preview(state: &AppState, token: &str) -> Result<Preview, LifecycleError> {
    let predicate = usable("invitation");
    let row = sqlx::query(AssertSqlSafe(format!(
        "{PREVIEW} WHERE invitation.token_hash = ? AND {predicate}"
    )))
    .bind(hash(token))
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(invitation_not_found)?;
    preview_from_row(&row)
}

fn preview_from_row(row: &SqliteRow) -> Result<Preview, LifecycleError> {
    Ok(Preview {
        room_id: row.try_get("room_id")?,
        room_name: row.try_get("name")?,
        role: Role::from_str(&row.try_get::<String, _>("role")?)?,
        inviter_email: row.try_get("inviter_email")?,
        expires_at: row.try_get("expires_at")?,
    })
}

/// What accepting an invitation reads of it: the predicate follows in each query.
const OFFER: &str = "SELECT invitation.id, invitation.room_id, invitation.role,
        invitation.email, room.name
    FROM invitation JOIN room ON room.id = invitation.room_id";

/// Joins `user` to the room the token invites to.
pub async fn accept(
    state: &AppState,
    token: &str,
    user: &db::User,
) -> Result<(RoomRow, Role), LifecycleError> {
    let predicate = usable("invitation");
    let row = sqlx::query(AssertSqlSafe(format!(
        "{OFFER} WHERE invitation.token_hash = ? AND {predicate}"
    )))
    .bind(hash(token))
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(invitation_not_found)?;
    join(state, &row, user).await
}

/// Joins `user` to the room invitation `id` invites to, when it is addressed to their
/// verified email; any other invitation is not found.
pub async fn accept_addressed(
    state: &AppState,
    id: i64,
    user: &db::User,
) -> Result<(RoomRow, Role), LifecycleError> {
    if !user.is_verified {
        return Err(invitation_not_found());
    }
    let predicate = usable("invitation");
    let row = sqlx::query(AssertSqlSafe(format!(
        "{OFFER} WHERE invitation.id = ? AND lower(invitation.email) = lower(?) AND {predicate}"
    )))
    .bind(id)
    .bind(&user.email)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(invitation_not_found)?;
    join(state, &row, user).await
}

/// Revokes invitation `id`, when it is addressed to `user`'s verified email and still usable.
pub async fn decline(state: &AppState, id: i64, user: &db::User) -> Result<(), LifecycleError> {
    if !user.is_verified {
        return Err(invitation_not_found());
    }
    let predicate = usable("invitation");
    let declined = sqlx::query(AssertSqlSafe(format!(
        "UPDATE invitation SET revoked = TRUE
         WHERE invitation.id = ? AND lower(invitation.email) = lower(?) AND {predicate}"
    )))
    .bind(id)
    .bind(&user.email)
    .execute(&state.pool)
    .await?;
    if declined.rows_affected() != 1 {
        return Err(invitation_not_found());
    }
    Ok(())
}

/// The usable invitations addressed to `user`'s email, newest first; none until the email
/// is verified.
pub async fn addressed_to(
    state: &AppState,
    user: &db::User,
) -> Result<Vec<(i64, Preview)>, LifecycleError> {
    if !user.is_verified {
        return Ok(Vec::new());
    }
    let predicate = usable("invitation");
    let rows = sqlx::query(AssertSqlSafe(format!(
        "{PREVIEW} WHERE lower(invitation.email) = lower(?) AND {predicate}
         ORDER BY invitation.created_at DESC, invitation.id DESC"
    )))
    .bind(&user.email)
    .fetch_all(&state.pool)
    .await?;
    rows.iter()
        .map(|row| Ok((row.try_get("id")?, preview_from_row(row)?)))
        .collect()
}

/// Joins `user` to the invited room, keeping the higher role if they are already a member
/// and closing the sockets they had there, which were admitted under the old role.
async fn join(
    state: &AppState,
    row: &SqliteRow,
    user: &db::User,
) -> Result<(RoomRow, Role), LifecycleError> {
    let id: i64 = row.try_get("id")?;
    let room_id: String = row.try_get("room_id")?;
    let role = Role::from_str(&row.try_get::<String, _>("role")?)?;
    let addressed: Option<String> = row.try_get("email")?;
    if let Some(addressed) = addressed
        && !addressed.eq_ignore_ascii_case(&user.email)
    {
        return Err(LifecycleError::Forbidden(
            "This invitation is for another email address".to_owned(),
        ));
    }
    let mut tx = state.pool.begin().await?;
    let granted = match join_with_one_use(&mut tx, id, &room_id, user.id, role).await {
        Ok(granted) => granted,
        Err(error) => {
            tx.rollback().await?;
            return Err(error);
        }
    };
    tx.commit().await?;
    state
        .rooms
        .revoke(&room_id, user.id, CloseReason::AccessChanged)
        .await;
    Ok((
        RoomRow {
            id: room_id,
            name: row.try_get("name")?,
        },
        granted,
    ))
}

pub async fn list(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
) -> Result<Vec<Invitation>, LifecycleError> {
    require_sharer(state, room_id, actor).await?;
    let predicate = usable("invitation");
    let rows = sqlx::query(AssertSqlSafe(format!(
        "SELECT invitation.id, invitation.email, invitation.role, invitation.expires_at,
                invitation.uses, invitation.max_uses
         FROM invitation
         WHERE invitation.room_id = ? AND {predicate}
         ORDER BY invitation.created_at DESC, invitation.id DESC"
    )))
    .bind(room_id)
    .fetch_all(&state.pool)
    .await?;
    rows.iter().map(invitation_from_row).collect()
}

pub async fn revoke(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
    invitation_id: i64,
) -> Result<(), LifecycleError> {
    require_sharer(state, room_id, actor).await?;
    let revoked = sqlx::query("UPDATE invitation SET revoked = TRUE WHERE id = ? AND room_id = ?")
        .bind(invitation_id)
        .bind(room_id)
        .execute(&state.pool)
        .await?;
    if revoked.rows_affected() != 1 {
        return Err(LifecycleError::NotFound(format!(
            "Invitation {invitation_id} not found"
        )));
    }
    Ok(())
}

/// Spends one use and grants the membership in the caller's transaction: a failure on either
/// side leaves the invitation as usable as it was, and a rolled back use is never spent.
async fn join_with_one_use(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: i64,
    room_id: &str,
    user: Uuid,
    role: Role,
) -> Result<Role, LifecycleError> {
    consume(tx, id).await?;
    Ok(db::raise_membership(tx, room_id, user, role).await?)
}

/// Spends one use in a single statement, so two accepts of a `max_uses = 1` token cannot
/// both succeed: the second update matches no row.
async fn consume(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: i64,
) -> Result<(), LifecycleError> {
    let predicate = usable("invitation");
    let used = sqlx::query(AssertSqlSafe(format!(
        "UPDATE invitation SET uses = uses + 1 WHERE invitation.id = ? AND {predicate}"
    )))
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if used.rows_affected() != 1 {
        return Err(invitation_not_found());
    }
    Ok(())
}

/// An unknown, expired, revoked or spent token is the same 404, as in Python.
pub(crate) fn invitation_not_found() -> LifecycleError {
    LifecycleError::NotFound("Invitation not found".to_owned())
}

fn invitation_from_row(row: &SqliteRow) -> Result<Invitation, LifecycleError> {
    Ok(Invitation {
        id: row.try_get("id")?,
        email: row.try_get("email")?,
        role: Role::from_str(&row.try_get::<String, _>("role")?)?,
        expires_at: row.try_get("expires_at")?,
        uses: row.try_get("uses")?,
        max_uses: row.try_get("max_uses")?,
    })
}

pub(crate) fn new_token() -> Result<(String, String), getrandom::Error> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes)?;
    let token = URL_SAFE_NO_PAD.encode(bytes);
    let hash = hash(&token);
    Ok((token, hash))
}

pub(crate) fn hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}
