use cocat_core::role::Role;
use cocat_server::db::{self, ProjectKind};
use cocat_server::room::RoomManager;
use cocat_server::rounds::{self, Kind, RoundRole, RoundState, Verdict, Visibility};
use cocat_server::{AppState, links::Links, ratelimit::LoginLimit};
use sqlx::SqlitePool;
use tempfile::TempDir;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

async fn pool() -> (SqlitePool, TempDir) {
    let dir = TempDir::new().unwrap();
    let pool = db::open(&dir.path().join("test.db")).await.unwrap();
    (pool, dir)
}

async fn user(pool: &SqlitePool, email: &str) -> Uuid {
    db::create_user(pool, email, "hunter2", false, true)
        .await
        .unwrap()
}

async fn project(pool: &SqlitePool, owner: Uuid) {
    db::create_room(pool, owner, "lab", "Lab", ProjectKind::Catalogues)
        .await
        .unwrap();
}

fn sorted(mut members: Vec<(Uuid, RoundRole)>) -> Vec<(Uuid, RoundRole)> {
    members.sort_by_key(|(id, role)| (*id, *role as u8));
    members
}

#[tokio::test]
async fn a_new_round_seats_owners_and_editors_and_its_creator_decides() {
    let (pool, _dir) = pool().await;
    let owner = user(&pool, "owner@x.org").await;
    let editor = user(&pool, "editor@x.org").await;
    let viewer = user(&pool, "viewer@x.org").await;
    project(&pool, owner).await;
    db::add_member(&pool, "lab", editor, Role::Editor)
        .await
        .unwrap();
    db::add_member(&pool, "lab", viewer, Role::Viewer)
        .await
        .unwrap();

    let id = rounds::create_round(
        &pool,
        "lab",
        rounds::RoundSpec {
            name: "R1".to_owned(),
            kind: Kind::Labelling,
            visibility: Visibility::Blind,
            deadline: None,
            budget: None,
        },
        editor,
    )
    .await
    .unwrap();

    let expected = sorted(vec![
        (owner, RoundRole::Proposer),
        (owner, RoundRole::Voter),
        (editor, RoundRole::Proposer),
        (editor, RoundRole::Voter),
        (editor, RoundRole::Decider),
    ]);
    assert_eq!(sorted(rounds::members(&pool, id).await.unwrap()), expected);
    let row = rounds::round(&pool, id).await.unwrap().unwrap();
    assert_eq!(row.state, RoundState::Proposing);
    assert_eq!(row.created_by, Some(editor));
}

#[tokio::test]
async fn deleting_the_project_deletes_its_rounds() {
    let (pool, _dir) = pool().await;
    let owner = user(&pool, "owner@x.org").await;
    project(&pool, owner).await;
    let id = rounds::create_round(
        &pool,
        "lab",
        rounds::RoundSpec {
            name: "R1".to_owned(),
            kind: Kind::Labelling,
            visibility: Visibility::Open,
            deadline: None,
            budget: None,
        },
        owner,
    )
    .await
    .unwrap();
    rounds::set_state(
        &pool,
        id,
        RoundState::Proposing,
        RoundState::Voting,
        None,
        Some(owner),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        rounds::log(&pool, id).await.unwrap()[0].what,
        "state: voting"
    );

    db::delete_room(&pool, "lab").await.unwrap();

    assert!(rounds::round(&pool, id).await.unwrap().is_none());
    assert!(rounds::members(&pool, id).await.unwrap().is_empty());
    assert!(rounds::log(&pool, id).await.unwrap().is_empty());
}

#[tokio::test]
async fn due_rounds_are_the_proposing_ones_past_their_deadline() {
    let (pool, _dir) = pool().await;
    let owner = user(&pool, "owner@x.org").await;
    project(&pool, owner).await;
    let make = |deadline: Option<&'static str>| {
        let pool = pool.clone();
        async move {
            rounds::create_round(
                &pool,
                "lab",
                rounds::RoundSpec {
                    name: "R".to_owned(),
                    kind: Kind::Labelling,
                    visibility: Visibility::Open,
                    deadline: deadline.map(str::to_owned),
                    budget: None,
                },
                owner,
            )
            .await
            .unwrap()
        }
    };
    let past = make(Some("2026-01-01T00:00:00Z")).await;
    let future = make(Some("2027-01-01T00:00:00Z")).await;
    let none = make(None).await;
    let moved = make(Some("2026-01-01T00:00:00Z")).await;
    rounds::set_state(
        &pool,
        moved,
        RoundState::Proposing,
        RoundState::Voting,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    let due = rounds::due_rounds(&pool, "2026-06-01T00:00:00Z")
        .await
        .unwrap();

    assert_eq!(due, vec![past]);
    let _ = (future, none);
}

fn app_state(pool: SqlitePool, dir: &TempDir) -> AppState {
    AppState {
        pool,
        secret: "test-secret".to_owned(),
        rooms: RoomManager::new(dir.path().join("rooms"), None),
        before_attach: None,
        login_limit: LoginLimit::default(),
        mailer: None,
        links: Links::default(),
        oidc: Vec::new(),
        web_dir: None,
        providers: Default::default(),
        http: Default::default(),
    }
}

fn instant(text: &str) -> OffsetDateTime {
    OffsetDateTime::parse(text, &Rfc3339).unwrap()
}

async fn round_with_deadline(pool: &SqlitePool, owner: Uuid, deadline: Option<&str>) -> Uuid {
    rounds::create_round(
        pool,
        "lab",
        rounds::RoundSpec {
            name: "R".to_owned(),
            kind: Kind::Labelling,
            visibility: Visibility::Open,
            deadline: deadline.map(str::to_owned),
            budget: None,
        },
        owner,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn a_past_deadline_moves_the_round_to_voting_by_itself() {
    let (pool, dir) = pool().await;
    let owner = user(&pool, "owner@x.org").await;
    project(&pool, owner).await;
    let id = round_with_deadline(&pool, owner, Some("2026-01-01T00:00:00Z")).await;
    let state = app_state(pool.clone(), &dir);

    let moved = rounds::close_due(&state, instant("2026-06-01T00:00:00Z"))
        .await
        .unwrap();

    assert_eq!(moved, 1);
    let row = rounds::round(&pool, id).await.unwrap().unwrap();
    assert_eq!(row.state, RoundState::Voting);
    let last = rounds::log(&pool, id).await.unwrap().pop().unwrap();
    assert_eq!((last.what.as_str(), last.actor), ("state: voting", None));
}

#[tokio::test]
async fn a_future_deadline_does_nothing() {
    let (pool, dir) = pool().await;
    let owner = user(&pool, "owner@x.org").await;
    project(&pool, owner).await;
    let id = round_with_deadline(&pool, owner, Some("2027-01-01T00:00:00Z")).await;
    let state = app_state(pool.clone(), &dir);

    let moved = rounds::close_due(&state, instant("2026-06-01T00:00:00Z"))
        .await
        .unwrap();

    assert_eq!(moved, 0);
    let row = rounds::round(&pool, id).await.unwrap().unwrap();
    assert_eq!(row.state, RoundState::Proposing);
}

#[tokio::test]
async fn a_round_without_deadline_is_never_moved() {
    let (pool, dir) = pool().await;
    let owner = user(&pool, "owner@x.org").await;
    project(&pool, owner).await;
    let id = round_with_deadline(&pool, owner, None).await;
    let state = app_state(pool.clone(), &dir);

    let moved = rounds::close_due(&state, instant("2099-01-01T00:00:00Z"))
        .await
        .unwrap();

    assert_eq!(moved, 0);
    let row = rounds::round(&pool, id).await.unwrap().unwrap();
    assert_eq!(row.state, RoundState::Proposing);
}

#[tokio::test]
async fn a_deadline_with_fractions_is_due_only_once_it_has_passed() {
    let (pool, _dir) = pool().await;
    let owner = user(&pool, "owner@x.org").await;
    project(&pool, owner).await;
    let id = round_with_deadline(&pool, owner, Some("2026-06-01T12:00:00.5Z")).await;

    let before = rounds::due_rounds(&pool, "2026-06-01T12:00:00Z")
        .await
        .unwrap();
    let after = rounds::due_rounds(&pool, "2026-06-01T12:00:01Z")
        .await
        .unwrap();

    assert_eq!((before, after), (vec![], vec![id]));
}

#[tokio::test]
async fn a_round_decided_before_the_deadline_closes_stays_decided() {
    let (pool, _dir) = pool().await;
    let owner = user(&pool, "owner@x.org").await;
    project(&pool, owner).await;
    let id = round_with_deadline(&pool, owner, Some("2026-01-01T00:00:00Z")).await;
    rounds::set_state(
        &pool,
        id,
        RoundState::Proposing,
        RoundState::Decided,
        Some("cat"),
        Some(owner),
        None,
    )
    .await
    .unwrap();

    let closed = rounds::close_if_proposing(&pool, id).await.unwrap();

    assert!(!closed);
    let row = rounds::round(&pool, id).await.unwrap().unwrap();
    assert_eq!(row.state, RoundState::Decided);
    let last = rounds::log(&pool, id).await.unwrap().pop().unwrap();
    assert_eq!(last.what, "state: decided");
}

#[tokio::test]
async fn moving_back_clears_a_deadline_that_has_passed_so_the_ticker_leaves_it() {
    let (pool, dir) = pool().await;
    let owner = user(&pool, "owner@x.org").await;
    project(&pool, owner).await;
    let id = round_with_deadline(&pool, owner, Some("2026-01-01T00:00:00Z")).await;
    let state = app_state(pool.clone(), &dir);
    rounds::set_state(
        &pool,
        id,
        RoundState::Proposing,
        RoundState::Voting,
        None,
        Some(owner),
        None,
    )
    .await
    .unwrap();

    rounds::set_state(
        &pool,
        id,
        RoundState::Voting,
        RoundState::Proposing,
        None,
        Some(owner),
        None,
    )
    .await
    .unwrap();
    let moved = rounds::close_due(&state, instant("2099-01-01T00:00:00Z"))
        .await
        .unwrap();

    assert_eq!(moved, 0);
    let row = rounds::round(&pool, id).await.unwrap().unwrap();
    assert_eq!((row.state, row.deadline), (RoundState::Proposing, None));
    let log = rounds::log(&pool, id).await.unwrap();
    let last: Vec<_> = log
        .iter()
        .rev()
        .take(2)
        .map(|l| (l.what.as_str(), l.actor))
        .collect();
    assert_eq!(
        last,
        [
            ("deadline cleared", Some(owner)),
            ("state: proposing", Some(owner))
        ]
    );
}

#[tokio::test]
async fn moving_back_keeps_a_deadline_still_ahead() {
    let (pool, _dir) = pool().await;
    let owner = user(&pool, "owner@x.org").await;
    project(&pool, owner).await;
    let id = round_with_deadline(&pool, owner, Some("2099-01-01T00:00:00Z")).await;
    rounds::set_state(
        &pool,
        id,
        RoundState::Proposing,
        RoundState::Voting,
        None,
        Some(owner),
        None,
    )
    .await
    .unwrap();

    rounds::set_state(
        &pool,
        id,
        RoundState::Voting,
        RoundState::Proposing,
        None,
        Some(owner),
        None,
    )
    .await
    .unwrap();

    let row = rounds::round(&pool, id).await.unwrap().unwrap();
    assert_eq!(row.deadline.as_deref(), Some("2099-01-01T00:00:00Z"));
}

#[tokio::test]
async fn a_move_from_a_stale_state_changes_and_logs_nothing() {
    let (pool, _dir) = pool().await;
    let owner = user(&pool, "owner@x.org").await;
    project(&pool, owner).await;
    let id = round_with_deadline(&pool, owner, None).await;
    rounds::close_if_proposing(&pool, id).await.unwrap();
    let before = rounds::log(&pool, id).await.unwrap().len();

    let moved = rounds::set_state(
        &pool,
        id,
        RoundState::Proposing,
        RoundState::Voting,
        None,
        Some(owner),
        None,
    )
    .await
    .unwrap();

    assert!(!moved);
    assert_eq!(rounds::log(&pool, id).await.unwrap().len(), before);
}

#[tokio::test]
async fn moving_off_decided_forgets_the_final_catalogue() {
    let (pool, _dir) = pool().await;
    let owner = user(&pool, "owner@x.org").await;
    project(&pool, owner).await;
    let id = round_with_deadline(&pool, owner, None).await;
    rounds::set_state(
        &pool,
        id,
        RoundState::Proposing,
        RoundState::Decided,
        Some("cat"),
        Some(owner),
        None,
    )
    .await
    .unwrap();

    rounds::set_state(
        &pool,
        id,
        RoundState::Decided,
        RoundState::Proposing,
        None,
        Some(owner),
        None,
    )
    .await
    .unwrap();

    let row = rounds::round(&pool, id).await.unwrap().unwrap();
    assert_eq!(row.final_catalogue, None);
}

#[tokio::test]
async fn deleting_an_account_keeps_its_proposals_and_votes() {
    let (pool, _dir) = pool().await;
    let owner = user(&pool, "owner@x.org").await;
    project(&pool, owner).await;
    let alice = user(&pool, "alice@x.org").await;
    db::add_member(&pool, "lab", alice, Role::Editor)
        .await
        .unwrap();
    let id = round_with_deadline(&pool, owner, None).await;
    let doc = rounds::add_proposal(&pool, id, alice).await.unwrap();
    rounds::set_verdicts(
        &pool,
        id,
        alice,
        1,
        &[("e1".to_owned(), Some(Verdict::Accept))],
    )
    .await
    .unwrap();

    db::delete_user(&pool, alice).await.unwrap();

    assert_eq!(rounds::proposals(&pool, id).await.unwrap(), [(alice, doc)]);
    assert_eq!(rounds::proposal_docs(&pool, "lab").await.unwrap(), [doc]);
    let kept = rounds::verdicts(&pool, id).await.unwrap();
    assert_eq!(
        kept.iter()
            .map(|row| (row.reviewer, row.verdict))
            .collect::<Vec<_>>(),
        [(alice, Verdict::Accept)]
    );
}

async fn round_of_kind(pool: &SqlitePool, owner: Uuid, kind: Kind) -> Uuid {
    rounds::create_round(
        pool,
        "lab",
        rounds::RoundSpec {
            name: "R".to_owned(),
            kind,
            visibility: Visibility::Open,
            deadline: None,
            budget: None,
        },
        owner,
    )
    .await
    .unwrap()
}

async fn go(pool: &SqlitePool, id: Uuid, from: RoundState, to: RoundState, note: Option<&str>) {
    let moved = rounds::set_state(pool, id, from, to, None, None, note)
        .await
        .unwrap();
    assert!(moved);
}

#[tokio::test]
async fn a_note_travels_with_the_move() {
    let (pool, _dir) = pool().await;
    let owner = user(&pool, "owner@x.org").await;
    project(&pool, owner).await;
    let id = round_of_kind(&pool, owner, Kind::Review).await;

    go(
        &pool,
        id,
        RoundState::Proposing,
        RoundState::Voting,
        Some("ready"),
    )
    .await;
    go(&pool, id, RoundState::Voting, RoundState::Decided, None).await;

    let log = rounds::log(&pool, id).await.unwrap();
    let notes: Vec<_> = log.iter().map(|l| l.note.as_deref()).collect();
    assert_eq!(notes, [Some("ready"), None]);
}

#[tokio::test]
async fn moving_back_starts_a_new_revision() {
    let (pool, _dir) = pool().await;
    let owner = user(&pool, "owner@x.org").await;
    project(&pool, owner).await;
    let id = round_of_kind(&pool, owner, Kind::Review).await;
    let revision = || async { rounds::round(&pool, id).await.unwrap().unwrap().revision };

    go(&pool, id, RoundState::Proposing, RoundState::Voting, None).await;
    let forward = revision().await;
    go(&pool, id, RoundState::Voting, RoundState::Proposing, None).await;
    let back = revision().await;
    go(&pool, id, RoundState::Proposing, RoundState::Decided, None).await;
    go(&pool, id, RoundState::Decided, RoundState::Voting, None).await;
    let sideways = revision().await;

    assert_eq!((forward, back, sideways), (1, 2, 2));
}

#[tokio::test]
async fn existing_rounds_get_a_kind() {
    let (pool, _dir) = pool().await;
    let owner = user(&pool, "owner@x.org").await;
    project(&pool, owner).await;
    let budgeted = round_of_kind(&pool, owner, Kind::Labelling).await;
    let plain = round_of_kind(&pool, owner, Kind::Review).await;
    sqlx::query("UPDATE round SET budget = '{}', kind = 'labelling' WHERE id = ?")
        .bind(budgeted.to_string())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE round SET kind = 'labelling' WHERE id = ?")
        .bind(plain.to_string())
        .execute(&pool)
        .await
        .unwrap();

    let backfill = include_str!("../migrations/0025_round_kinds.sql");
    let statement = backfill
        .lines()
        .find(|line| line.starts_with("UPDATE round SET kind"))
        .unwrap();
    sqlx::query(statement).execute(&pool).await.unwrap();

    let kinds = (
        rounds::round(&pool, budgeted).await.unwrap().unwrap().kind,
        rounds::round(&pool, plain).await.unwrap().unwrap().kind,
    );
    assert_eq!(kinds, (Kind::Downlink, Kind::Labelling));
}
