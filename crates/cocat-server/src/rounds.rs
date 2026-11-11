//! Selection rounds: who proposes, votes and decides, and what each of them may see.
//!
//! Proposals are documents of the project; this module stores the round around them.

use std::time::Duration;

use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::db::{self, DbError};
use crate::{AppState, ws};

const DEADLINES_EVERY: Duration = Duration::from_secs(60);

macro_rules! text_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident),+ }) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, sqlx::Type,
            serde::Serialize, serde::Deserialize, utoipa::ToSchema,
        )]
        #[sqlx(type_name = "TEXT", rename_all = "lowercase")]
        #[serde(rename_all = "lowercase")]
        #[schema(rename_all = "lowercase")]
        pub enum $name { $($variant),+ }
    };
}

text_enum!(
    /// Declared in the order a round goes through them.
    RoundState { Proposing, Voting, Decided }
);
text_enum!(Visibility { Open, Blind });
text_enum!(RoundRole {
    Proposer,
    Voter,
    Decider
});

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct RoundRow {
    pub id: Uuid,
    pub room_id: String,
    pub name: String,
    pub state: RoundState,
    pub visibility: Visibility,
    pub deadline: Option<String>,
    pub budget: Option<String>,
    pub final_catalogue: Option<String>,
    pub created_by: Option<Uuid>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct LogLine {
    pub at: String,
    pub actor: Option<Uuid>,
    pub what: String,
}

/// A change to a round's settings; `None` leaves a field alone, `Some(None)` clears it.
#[derive(Debug, Default, Clone)]
pub struct RoundPatch {
    pub name: Option<String>,
    pub visibility: Option<Visibility>,
    pub deadline: Option<Option<String>>,
    pub budget: Option<Option<String>>,
}

pub mod rules {
    use cocat_core::role::Role;

    use super::{RoundRole, RoundRow, RoundState, Visibility};

    /// A user's place in a round: their role in the project and their roles in the round.
    pub struct Seat {
        pub project_role: Role,
        pub roles: Vec<RoundRole>,
    }

    impl Seat {
        fn holds(&self, role: RoundRole) -> bool {
            self.roles.contains(&role)
        }

        fn owns_project(&self) -> bool {
            self.project_role == Role::Owner
        }

        /// A decider only while their project role can still write: a demotion ends it.
        fn is_decider(&self) -> bool {
            self.holds(RoundRole::Decider) && self.project_role.can_write()
        }
    }

    /// The role a user holds on a proposal document right now, if any.
    pub fn proposal_role(round: &RoundRow, seat: &Seat, is_author: bool) -> Option<Role> {
        let writes =
            is_author && seat.holds(RoundRole::Proposer) && round.state == RoundState::Proposing;
        if writes {
            Some(Role::Editor)
        } else {
            may_read_proposal(round, seat, is_author).then_some(Role::Viewer)
        }
    }

    fn may_read_proposal(round: &RoundRow, seat: &Seat, is_author: bool) -> bool {
        match round.visibility {
            Visibility::Open => true,
            Visibility::Blind => {
                round.state != RoundState::Proposing
                    || is_author
                    || seat.is_decider()
                    || seat.owns_project()
            }
        }
    }

    pub fn may_read_votes(round: &RoundRow, seat: &Seat, is_voter_self: bool) -> bool {
        match round.visibility {
            Visibility::Open => true,
            Visibility::Blind => {
                round.state == RoundState::Decided
                    || is_voter_self
                    || seat.is_decider()
                    || seat.owns_project()
            }
        }
    }

    /// Who may change a round's settings and members.
    pub fn may_administer(seat: &Seat) -> bool {
        seat.owns_project() || seat.is_decider()
    }

    /// Deciders move a round forward; only a project owner moves it back.
    pub fn may_move(round: &RoundRow, seat: &Seat, to: RoundState) -> bool {
        match to.cmp(&round.state) {
            std::cmp::Ordering::Greater => seat.is_decider(),
            std::cmp::Ordering::Less => seat.owns_project(),
            std::cmp::Ordering::Equal => false,
        }
    }
}

fn parse_uuid(text: &str) -> Result<Uuid, sqlx::Error> {
    Uuid::parse_str(text).map_err(|error| sqlx::Error::Decode(Box::new(error)))
}

fn parse_opt_uuid(text: Option<String>) -> Result<Option<Uuid>, sqlx::Error> {
    text.as_deref().map(parse_uuid).transpose()
}

fn round_from_row(row: &SqliteRow) -> Result<RoundRow, sqlx::Error> {
    Ok(RoundRow {
        id: parse_uuid(&row.try_get::<String, _>("id")?)?,
        room_id: row.try_get("room_id")?,
        name: row.try_get("name")?,
        state: row.try_get("state")?,
        visibility: row.try_get("visibility")?,
        deadline: row.try_get("deadline")?,
        budget: row.try_get("budget")?,
        final_catalogue: row.try_get("final_catalogue")?,
        created_by: parse_opt_uuid(row.try_get("created_by")?)?,
        created_at: row.try_get("created_at")?,
    })
}

macro_rules! select_round {
    ($tail:literal) => {
        concat!(
            "SELECT id, room_id, name, state, visibility, deadline, budget, final_catalogue, ",
            "created_by, CAST(created_at AS TEXT) AS created_at FROM round ",
            $tail
        )
    };
}

/// Everyone who may edit the project: direct members and members of groups it is shared with.
async fn project_writers(pool: &SqlitePool, room_id: &str) -> Result<Vec<Uuid>, DbError> {
    let candidates: Vec<String> = sqlx::query_scalar(
        r#"SELECT user_id FROM membership WHERE room_id = ?1
           UNION
           SELECT group_member.user_id FROM room_group
             JOIN group_member ON group_member.group_id = room_group.group_id
           WHERE room_group.room_id = ?1"#,
    )
    .bind(room_id)
    .fetch_all(pool)
    .await?;
    let mut writers = Vec::new();
    for candidate in candidates {
        let user = parse_uuid(&candidate)?;
        if db::role_in(pool, room_id, user)
            .await?
            .is_some_and(|role| role.can_write())
        {
            writers.push(user);
        }
    }
    Ok(writers)
}

async fn insert_member(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    round_id: Uuid,
    user: Uuid,
    role: RoundRole,
) -> Result<(), DbError> {
    sqlx::query("INSERT OR IGNORE INTO round_member (round_id, user_id, role) VALUES (?, ?, ?)")
        .bind(round_id.to_string())
        .bind(user.to_string())
        .bind(role)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Creates a round in `proposing`; every project owner and editor proposes and votes, and
/// `by` decides.
pub async fn create_round(
    pool: &SqlitePool,
    room_id: &str,
    name: &str,
    visibility: Visibility,
    deadline: Option<String>,
    budget: Option<String>,
    by: Uuid,
) -> Result<Uuid, DbError> {
    let writers = project_writers(pool, room_id).await?;
    let id = Uuid::new_v4();
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO round (id, room_id, name, visibility, deadline, budget, created_by)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind(room_id)
    .bind(name)
    .bind(visibility)
    .bind(deadline)
    .bind(budget)
    .bind(by.to_string())
    .execute(&mut *tx)
    .await?;
    for writer in writers {
        insert_member(&mut tx, id, writer, RoundRole::Proposer).await?;
        insert_member(&mut tx, id, writer, RoundRole::Voter).await?;
    }
    insert_member(&mut tx, id, by, RoundRole::Decider).await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn round(pool: &SqlitePool, id: Uuid) -> Result<Option<RoundRow>, DbError> {
    let row = sqlx::query(select_round!("WHERE id = ?"))
        .bind(id.to_string())
        .fetch_optional(pool)
        .await?;
    Ok(row.as_ref().map(round_from_row).transpose()?)
}

pub async fn rounds_of(pool: &SqlitePool, room_id: &str) -> Result<Vec<RoundRow>, DbError> {
    let rows = sqlx::query(select_round!("WHERE room_id = ? ORDER BY created_at, id"))
        .bind(room_id)
        .fetch_all(pool)
        .await?;
    Ok(rows.iter().map(round_from_row).collect::<Result<_, _>>()?)
}

pub async fn update_round(pool: &SqlitePool, id: Uuid, patch: RoundPatch) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE round SET
           name = COALESCE(?2, name),
           visibility = COALESCE(?3, visibility),
           deadline = CASE WHEN ?4 THEN ?5 ELSE deadline END,
           budget = CASE WHEN ?6 THEN ?7 ELSE budget END
         WHERE id = ?1",
    )
    .bind(id.to_string())
    .bind(patch.name)
    .bind(patch.visibility)
    .bind(patch.deadline.is_some())
    .bind(patch.deadline.flatten())
    .bind(patch.budget.is_some())
    .bind(patch.budget.flatten())
    .execute(pool)
    .await?;
    Ok(())
}

fn state_name(state: RoundState) -> &'static str {
    match state {
        RoundState::Proposing => "proposing",
        RoundState::Voting => "voting",
        RoundState::Decided => "decided",
    }
}

/// Moves the round from `from` and writes the move in its history. `false`, and nothing
/// written, when the round is no longer in `from`.
pub async fn set_state(
    pool: &SqlitePool,
    id: Uuid,
    from: RoundState,
    state: RoundState,
    final_catalogue: Option<&str>,
    actor: Option<Uuid>,
) -> Result<bool, DbError> {
    let mut tx = pool.begin().await?;
    let moved = sqlx::query(
        "UPDATE round SET state = ?2,
           final_catalogue = CASE WHEN ?2 = 'decided'
             THEN COALESCE(?3, final_catalogue) ELSE NULL END
         WHERE id = ?1 AND state = ?4",
    )
    .bind(id.to_string())
    .bind(state)
    .bind(final_catalogue)
    .bind(from)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;
    if moved {
        log_line(&mut tx, id, actor, &format!("state: {}", state_name(state))).await?;
        if state == RoundState::Proposing {
            clear_passed_deadline(&mut tx, id, actor).await?;
        }
    }
    tx.commit().await?;
    Ok(moved)
}

async fn log_line(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: Uuid,
    actor: Option<Uuid>,
    what: &str,
) -> Result<(), DbError> {
    sqlx::query("INSERT INTO round_log (round_id, actor, what) VALUES (?, ?, ?)")
        .bind(id.to_string())
        .bind(actor.map(|actor| actor.to_string()))
        .bind(what)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// A round back in proposing with a deadline already behind it would be closed again by the
/// next tick, undoing the move.
async fn clear_passed_deadline(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: Uuid,
    actor: Option<Uuid>,
) -> Result<(), DbError> {
    let cleared = sqlx::query(
        "UPDATE round SET deadline = NULL
         WHERE id = ? AND deadline IS NOT NULL AND julianday(deadline) <= julianday('now')",
    )
    .bind(id.to_string())
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if cleared > 0 {
        log_line(tx, id, actor, "deadline cleared").await?;
    }
    Ok(())
}

pub async fn members(pool: &SqlitePool, id: Uuid) -> Result<Vec<(Uuid, RoundRole)>, DbError> {
    let rows = sqlx::query("SELECT user_id, role FROM round_member WHERE round_id = ?")
        .bind(id.to_string())
        .fetch_all(pool)
        .await?;
    Ok(rows
        .iter()
        .map(|row| {
            Ok((
                parse_uuid(&row.try_get::<String, _>("user_id")?)?,
                row.try_get("role")?,
            ))
        })
        .collect::<Result<_, sqlx::Error>>()?)
}

/// The round roles `user` holds among the round's members.
pub fn roles_of(members: &[(Uuid, RoundRole)], user: Uuid) -> Vec<RoundRole> {
    members
        .iter()
        .filter(|(member, _)| *member == user)
        .map(|(_, role)| *role)
        .collect()
}

pub async fn set_members(
    pool: &SqlitePool,
    id: Uuid,
    members: &[(Uuid, RoundRole)],
) -> Result<(), DbError> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM round_member WHERE round_id = ?")
        .bind(id.to_string())
        .execute(&mut *tx)
        .await?;
    for (user, role) in members {
        insert_member(&mut tx, id, *user, *role).await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn proposal_of(
    pool: &SqlitePool,
    id: Uuid,
    author: Uuid,
) -> Result<Option<Uuid>, DbError> {
    let doc: Option<String> =
        sqlx::query_scalar("SELECT doc_id FROM round_proposal WHERE round_id = ? AND author = ?")
            .bind(id.to_string())
            .bind(author.to_string())
            .fetch_optional(pool)
            .await?;
    Ok(parse_opt_uuid(doc)?)
}

/// Gives `author` their proposal document in the round, or returns the one they have.
pub async fn add_proposal(pool: &SqlitePool, id: Uuid, author: Uuid) -> Result<Uuid, DbError> {
    sqlx::query("INSERT OR IGNORE INTO round_proposal (round_id, author, doc_id) VALUES (?, ?, ?)")
        .bind(id.to_string())
        .bind(author.to_string())
        .bind(Uuid::new_v4().to_string())
        .execute(pool)
        .await?;
    proposal_of(pool, id, author)
        .await?
        .ok_or(DbError::Sqlx(sqlx::Error::RowNotFound))
}

pub async fn proposals(pool: &SqlitePool, id: Uuid) -> Result<Vec<(Uuid, Uuid)>, DbError> {
    let rows = sqlx::query(
        "SELECT author, doc_id FROM round_proposal WHERE round_id = ? ORDER BY created_at, author",
    )
    .bind(id.to_string())
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|row| {
            Ok((
                parse_uuid(&row.try_get::<String, _>("author")?)?,
                parse_uuid(&row.try_get::<String, _>("doc_id")?)?,
            ))
        })
        .collect::<Result<_, sqlx::Error>>()?)
}

/// A proposal document of a project, as the document list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectProposal {
    pub doc: Uuid,
    pub author: Uuid,
    pub created_at: String,
}

/// Every proposal of every round of the project, oldest first.
pub async fn project_proposals(
    pool: &SqlitePool,
    room_id: &str,
) -> Result<Vec<ProjectProposal>, DbError> {
    let rows = sqlx::query(
        "SELECT round_proposal.doc_id, round_proposal.author,
                CAST(round_proposal.created_at AS TEXT) AS created_at
         FROM round_proposal JOIN round ON round.id = round_proposal.round_id
         WHERE round.room_id = ? ORDER BY round_proposal.created_at, round_proposal.doc_id",
    )
    .bind(room_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|row| {
            Ok(ProjectProposal {
                doc: parse_uuid(&row.try_get::<String, _>("doc_id")?)?,
                author: parse_uuid(&row.try_get::<String, _>("author")?)?,
                created_at: row.try_get("created_at")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()?)
}

/// The document ids of every proposal of the project's rounds; they share its storage.
pub async fn proposal_docs(
    executor: impl sqlx::Executor<'_, Database = sqlx::Sqlite>,
    room_id: &str,
) -> Result<Vec<Uuid>, DbError> {
    let docs: Vec<String> = sqlx::query_scalar(
        "SELECT round_proposal.doc_id FROM round_proposal
         JOIN round ON round.id = round_proposal.round_id WHERE round.room_id = ?",
    )
    .bind(room_id)
    .fetch_all(executor)
    .await?;
    Ok(docs
        .iter()
        .map(|doc| parse_uuid(doc))
        .collect::<Result<_, _>>()?)
}

/// The round a project's document is a proposal of, and its author.
pub async fn proposal_by_doc(
    pool: &SqlitePool,
    room_id: &str,
    doc: Uuid,
) -> Result<Option<(RoundRow, Uuid)>, DbError> {
    let author: Option<(String, String)> = sqlx::query_as(
        "SELECT round_proposal.round_id, round_proposal.author FROM round_proposal
         JOIN round ON round.id = round_proposal.round_id
         WHERE round.room_id = ? AND round_proposal.doc_id = ?",
    )
    .bind(room_id)
    .bind(doc.to_string())
    .fetch_optional(pool)
    .await?;
    let Some((round_id, author)) = author else {
        return Ok(None);
    };
    let row = round(pool, parse_uuid(&round_id)?).await?;
    Ok(row
        .map(|row| Ok::<_, sqlx::Error>((row, parse_uuid(&author)?)))
        .transpose()?)
}

/// Replaces the voter's supported events.
pub async fn set_votes(
    pool: &SqlitePool,
    id: Uuid,
    voter: Uuid,
    events: &[String],
) -> Result<(), DbError> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM round_vote WHERE round_id = ? AND voter = ?")
        .bind(id.to_string())
        .bind(voter.to_string())
        .execute(&mut *tx)
        .await?;
    for event in events {
        sqlx::query("INSERT OR IGNORE INTO round_vote (round_id, voter, event) VALUES (?, ?, ?)")
            .bind(id.to_string())
            .bind(voter.to_string())
            .bind(event)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn votes(pool: &SqlitePool, id: Uuid) -> Result<Vec<(Uuid, String)>, DbError> {
    let rows =
        sqlx::query("SELECT voter, event FROM round_vote WHERE round_id = ? ORDER BY voter, event")
            .bind(id.to_string())
            .fetch_all(pool)
            .await?;
    Ok(rows
        .iter()
        .map(|row| {
            Ok((
                parse_uuid(&row.try_get::<String, _>("voter")?)?,
                row.try_get("event")?,
            ))
        })
        .collect::<Result<_, sqlx::Error>>()?)
}

pub async fn log(pool: &SqlitePool, id: Uuid) -> Result<Vec<LogLine>, DbError> {
    let rows = sqlx::query(
        "SELECT CAST(at AS TEXT) AS at, actor, what FROM round_log WHERE round_id = ? ORDER BY rowid",
    )
    .bind(id.to_string())
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|row| {
            Ok(LogLine {
                at: row.try_get("at")?,
                actor: parse_opt_uuid(row.try_get("actor")?)?,
                what: row.try_get("what")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()?)
}

/// Rounds still proposing whose deadline has passed. Compared as instants, not as text:
/// as text "...:00.5Z" sorts before "...:00Z".
pub async fn due_rounds(pool: &SqlitePool, now: &str) -> Result<Vec<Uuid>, DbError> {
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM round WHERE state = 'proposing' AND deadline IS NOT NULL AND julianday(deadline) <= julianday(?)",
    )
    .bind(now)
    .fetch_all(pool)
    .await?;
    Ok(ids
        .iter()
        .map(|id| parse_uuid(id))
        .collect::<Result<_, _>>()?)
}

/// Moves every round whose deadline has passed to voting, as the server and not a person.
/// Returns how many it moved.
pub async fn close_due(state: &AppState, now: OffsetDateTime) -> Result<usize, DbError> {
    let now = now
        .format(&Rfc3339)
        .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
    let mut moved = 0;
    for id in due_rounds(&state.pool, &now).await? {
        if close_if_proposing(&state.pool, id).await? {
            moved += 1;
            if let Some(row) = round(&state.pool, id).await? {
                ws::check_room_roles(state, &row.room_id).await;
            }
        }
    }
    Ok(moved)
}

/// Moves a round to voting and logs it, only if it is still proposing: a decider may have
/// moved it since the deadline was seen. `false` when nothing changed.
pub async fn close_if_proposing(pool: &SqlitePool, id: Uuid) -> Result<bool, DbError> {
    let mut tx = pool.begin().await?;
    let changed =
        sqlx::query("UPDATE round SET state = 'voting' WHERE id = ? AND state = 'proposing'")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?
            .rows_affected();
    if changed > 0 {
        sqlx::query("INSERT INTO round_log (round_id, actor, what) VALUES (?, NULL, ?)")
            .bind(id.to_string())
            .bind(format!("state: {}", state_name(RoundState::Voting)))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(changed > 0)
}

/// Wakes every minute, like the relay scheduler.
pub fn spawn_deadlines(state: AppState) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(DEADLINES_EVERY);
        loop {
            ticker.tick().await;
            if let Err(error) = close_due(&state, OffsetDateTime::now_utc()).await {
                tracing::warn!(%error, "closing the rounds past their deadline failed");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use cocat_core::role::Role;

    use super::rules::*;
    use super::*;

    fn round_in(state: RoundState, visibility: Visibility) -> RoundRow {
        RoundRow {
            id: Uuid::nil(),
            room_id: "lab".into(),
            name: "R".into(),
            state,
            visibility,
            deadline: None,
            budget: None,
            final_catalogue: None,
            created_by: None,
            created_at: String::new(),
        }
    }

    fn seat(project_role: Role, roles: &[RoundRole]) -> Seat {
        Seat {
            project_role,
            roles: roles.to_vec(),
        }
    }

    const PROPOSER: &[RoundRole] = &[RoundRole::Proposer, RoundRole::Voter];

    #[test]
    fn the_author_writes_only_while_proposing() {
        let author = seat(Role::Editor, PROPOSER);
        let proposing = round_in(RoundState::Proposing, Visibility::Open);
        let voting = round_in(RoundState::Voting, Visibility::Open);
        assert_eq!(proposal_role(&proposing, &author, true), Some(Role::Editor));
        assert_eq!(proposal_role(&voting, &author, true), Some(Role::Viewer));
        assert_eq!(
            proposal_role(&proposing, &author, false),
            Some(Role::Viewer)
        );
    }

    #[test]
    fn a_removed_proposer_reads_but_no_longer_writes() {
        let removed = seat(Role::Editor, &[RoundRole::Voter]);
        let round = round_in(RoundState::Proposing, Visibility::Blind);
        assert_eq!(proposal_role(&round, &removed, true), Some(Role::Viewer));
    }

    #[test]
    fn a_blind_proposal_is_read_by_author_deciders_and_owners_only() {
        let round = round_in(RoundState::Proposing, Visibility::Blind);
        let editor = seat(Role::Editor, PROPOSER);
        let decider = seat(Role::Editor, &[RoundRole::Decider]);
        let owner = seat(Role::Owner, &[]);
        assert_eq!(proposal_role(&round, &editor, false), None);
        assert_eq!(proposal_role(&round, &decider, false), Some(Role::Viewer));
        assert_eq!(proposal_role(&round, &owner, false), Some(Role::Viewer));
        assert_eq!(proposal_role(&round, &editor, true), Some(Role::Editor));
    }

    #[test]
    fn every_member_reads_a_blind_proposal_once_proposing_ends() {
        let editor = seat(Role::Editor, PROPOSER);
        for state in [RoundState::Voting, RoundState::Decided] {
            let round = round_in(state, Visibility::Blind);
            assert_eq!(proposal_role(&round, &editor, false), Some(Role::Viewer));
        }
    }

    #[test]
    fn an_open_proposal_is_read_by_every_member() {
        let round = round_in(RoundState::Proposing, Visibility::Open);
        let viewer = seat(Role::Viewer, &[]);
        assert_eq!(proposal_role(&round, &viewer, false), Some(Role::Viewer));
    }

    #[test]
    fn blind_votes_are_hidden_until_decided() {
        let voter = seat(Role::Editor, PROPOSER);
        let decider = seat(Role::Editor, &[RoundRole::Decider]);
        let voting = round_in(RoundState::Voting, Visibility::Blind);
        let decided = round_in(RoundState::Decided, Visibility::Blind);
        assert!(!may_read_votes(&voting, &voter, false));
        assert!(may_read_votes(&voting, &voter, true));
        assert!(may_read_votes(&voting, &decider, false));
        assert!(may_read_votes(&voting, &seat(Role::Owner, &[]), false));
        assert!(may_read_votes(&decided, &voter, false));
    }

    #[test]
    fn deciders_move_forward_and_owners_move_back() {
        let decider = seat(Role::Editor, &[RoundRole::Decider]);
        let owner = seat(Role::Owner, &[]);
        let proposing = round_in(RoundState::Proposing, Visibility::Open);
        let voting = round_in(RoundState::Voting, Visibility::Open);
        let decided = round_in(RoundState::Decided, Visibility::Open);
        assert!(may_move(&proposing, &decider, RoundState::Decided));
        assert!(!may_move(&voting, &decider, RoundState::Proposing));
        assert!(may_move(&decided, &owner, RoundState::Proposing));
        assert!(!may_move(&proposing, &owner, RoundState::Voting));
    }

    #[test]
    fn a_decider_who_cannot_edit_the_project_decides_nothing() {
        let demoted = seat(Role::Viewer, &[RoundRole::Decider]);
        let proposing = round_in(RoundState::Proposing, Visibility::Open);
        assert!(!may_move(&proposing, &demoted, RoundState::Voting));
        assert!(!may_administer(&demoted));
        assert!(may_administer(&seat(Role::Editor, &[RoundRole::Decider])));
        assert!(may_administer(&seat(Role::Owner, &[])));
    }
}
