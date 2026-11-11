use cocat_core::role::Role;
use cocat_server::db::{self, ProjectKind};
use cocat_server::rounds::{self, RoundRole, RoundState, Visibility};
use sqlx::SqlitePool;
use tempfile::TempDir;
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

    let id = rounds::create_round(&pool, "lab", "R1", Visibility::Blind, None, None, editor)
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
    let id = rounds::create_round(&pool, "lab", "R1", Visibility::Open, None, None, owner)
        .await
        .unwrap();
    rounds::set_state(&pool, id, RoundState::Voting, None, Some(owner))
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
                "R",
                Visibility::Open,
                deadline.map(str::to_owned),
                None,
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
    rounds::set_state(&pool, moved, RoundState::Voting, None, None)
        .await
        .unwrap();

    let due = rounds::due_rounds(&pool, "2026-06-01T00:00:00Z")
        .await
        .unwrap();

    assert_eq!(due, vec![past]);
    let _ = (future, none);
}
