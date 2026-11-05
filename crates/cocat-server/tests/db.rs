use cocat_core::role::Role;
use cocat_server::db;
use sqlx::{Row, SqlitePool};
use tempfile::TempDir;
use uuid::Uuid;

async fn seeded() -> (TempDir, SqlitePool) {
    let dir = TempDir::new().unwrap();
    let pool = db::open(&dir.path().join("test.db")).await.unwrap();
    (dir, pool)
}

async fn user(pool: &SqlitePool, email: &str) -> Uuid {
    db::create_user(pool, email, "hunter2", false)
        .await
        .unwrap()
}

async fn role(pool: &SqlitePool, room_id: &str, user_id: Uuid) -> Option<Role> {
    db::role_in(pool, room_id, user_id).await.unwrap()
}

#[tokio::test]
async fn migrations_create_the_tables_on_a_new_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.db");
    db::open(&path).await.unwrap().close().await;

    let pool = db::open(&path).await.unwrap();

    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(applied, 1, "the second open must not run a migration twice");
    let owner = user(&pool, "owner@example.org").await;
    db::create_room(&pool, owner, "room-1", "Room one")
        .await
        .unwrap();
}

#[tokio::test]
async fn a_second_owner_is_rejected_by_the_schema() {
    let (_dir, pool) = seeded().await;
    let owner = user(&pool, "owner@example.org").await;
    let other = user(&pool, "other@example.org").await;
    db::create_room(&pool, owner, "room-1", "Room one")
        .await
        .unwrap();

    let promoted = db::add_member(&pool, "room-1", other, Role::Owner).await;

    assert!(promoted.is_err(), "two owners must be impossible");
    assert_eq!(role(&pool, "room-1", owner).await, Some(Role::Owner));
    assert_eq!(role(&pool, "room-1", other).await, None);
}

#[tokio::test]
async fn set_owner_moves_the_owner_role() {
    let (_dir, pool) = seeded().await;
    let owner = user(&pool, "owner@example.org").await;
    let other = user(&pool, "other@example.org").await;
    db::create_room(&pool, owner, "room-1", "Room one")
        .await
        .unwrap();
    db::add_member(&pool, "room-1", other, Role::Editor)
        .await
        .unwrap();

    db::set_owner(&pool, "room-1", other).await.unwrap();

    assert_eq!(role(&pool, "room-1", owner).await, Some(Role::Editor));
    assert_eq!(role(&pool, "room-1", other).await, Some(Role::Owner));
}

#[tokio::test]
async fn emails_match_case_insensitively() {
    let (_dir, pool) = seeded().await;
    let id = db::create_user(&pool, "Alice@X.org", "hunter2", false)
        .await
        .unwrap();

    let found = db::user_by_email(&pool, "alice@x.org").await.unwrap();
    let again = db::create_user(&pool, "alice@x.org", "hunter2", false).await;

    assert_eq!(found.map(|user| user.id), Some(id));
    assert!(
        again.is_err(),
        "the same email in another case must be taken"
    );
}

#[tokio::test]
async fn deleting_a_room_removes_its_memberships() {
    let (_dir, pool) = seeded().await;
    let owner = user(&pool, "owner@example.org").await;
    let other = user(&pool, "other@example.org").await;
    db::create_room(&pool, owner, "room-1", "Room one")
        .await
        .unwrap();
    db::add_member(&pool, "room-1", other, Role::Viewer)
        .await
        .unwrap();

    let deleted = db::delete_room(&pool, "room-1").await.unwrap();
    let memberships: i64 =
        sqlx::query_scalar("SELECT count(*) FROM membership WHERE room_id = 'room-1'")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert!(deleted);
    assert_eq!(memberships, 0);
    assert!(!db::delete_room(&pool, "room-1").await.unwrap());
}

#[tokio::test]
async fn the_schema_is_the_phase_one_design() {
    let (_dir, pool) = seeded().await;
    let tables = [
        (
            "PRAGMA table_info(\"user\")",
            &[
                "id",
                "email",
                "hashed_password",
                "is_active",
                "is_verified",
                "is_superuser",
                "created_at",
            ][..],
        ),
        (
            "PRAGMA table_info(room)",
            &["id", "name", "created_at", "created_by"][..],
        ),
        (
            "PRAGMA table_info(membership)",
            &["room_id", "user_id", "role", "joined_at"][..],
        ),
    ];
    for (query, columns) in tables {
        let rows = sqlx::query(query).fetch_all(&pool).await.unwrap();
        let names: Vec<String> = rows.iter().map(|row| row.get("name")).collect();
        for column in columns {
            assert!(
                names.iter().any(|name| name == column),
                "{column} is missing from {query}: {names:?}"
            );
        }
    }

    let indexes: Vec<String> = sqlx::query("PRAGMA index_list(membership)")
        .fetch_all(&pool)
        .await
        .unwrap()
        .iter()
        .map(|row| row.get("name"))
        .collect();
    assert!(
        indexes.iter().any(|name| name == "one_owner_per_room"),
        "{indexes:?} has no one-owner index"
    );
}
