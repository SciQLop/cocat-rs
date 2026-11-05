use std::path::Path;
use std::process::Command;

use cocat_core::role::Role;
use cocat_server::db;
use sqlx::{Row, SqlitePool};
use tempfile::TempDir;

async fn seeded_db() -> (TempDir, SqlitePool) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let pool = db::open_writable(&db_path).await.unwrap();
    db::ensure_schema(&pool).await.unwrap();
    (dir, pool)
}

#[tokio::test]
async fn create_user_then_read_it_back() {
    let (_dir, pool) = seeded_db().await;

    let id = db::create_user(&pool, "user@example.org", "hunter2", false)
        .await
        .unwrap();
    let user = db::user_by_email(&pool, "user@example.org")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(user.id, id);
    assert_eq!(user.email, "user@example.org");
    assert!(user.is_active);
    assert!(user.hashed_password.starts_with("$argon2"));
    assert!(
        db::user_by_email(&pool, "missing@example.org")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn create_room_assigns_the_owner() {
    let (_dir, pool) = seeded_db().await;
    let owner = db::create_user(&pool, "owner@example.org", "hunter2", false)
        .await
        .unwrap();

    db::create_room(&pool, owner, "room-1", "Room one")
        .await
        .unwrap();

    assert_eq!(
        db::role_in(&pool, "room-1", owner).await.unwrap(),
        Some(Role::Owner)
    );
}

#[tokio::test]
async fn add_user_to_room_sets_the_role() {
    let (_dir, pool) = seeded_db().await;
    let owner = db::create_user(&pool, "owner@example.org", "hunter2", false)
        .await
        .unwrap();
    let viewer = db::create_user(&pool, "viewer@example.org", "hunter2", false)
        .await
        .unwrap();
    db::create_room(&pool, owner, "room-1", "Room one")
        .await
        .unwrap();

    db::add_member(&pool, "room-1", viewer, Role::Viewer)
        .await
        .unwrap();

    assert_eq!(
        db::role_in(&pool, "room-1", viewer).await.unwrap(),
        Some(Role::Viewer)
    );
}

#[tokio::test]
async fn the_schema_matches_the_python_columns() {
    let (_dir, pool) = seeded_db().await;

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
                "rooms",
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
}

#[tokio::test]
async fn the_cli_creates_a_database_the_server_can_read() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    run_cli(
        &db_path,
        &[
            "create-user",
            "--email",
            "owner@example.org",
            "--password",
            "hunter2",
        ],
    );
    run_cli(
        &db_path,
        &[
            "create-user",
            "--email",
            "viewer@example.org",
            "--password",
            "hunter2",
        ],
    );
    run_cli(
        &db_path,
        &[
            "create-room",
            "--owner",
            "owner@example.org",
            "--room-id",
            "cli-room",
        ],
    );
    run_cli(
        &db_path,
        &[
            "add-user-to-room",
            "--email",
            "viewer@example.org",
            "--room_id",
            "cli-room",
            "--role",
            "viewer",
        ],
    );

    let pool = db::open_read_only(&db_path).await.unwrap();
    let owner = db::user_by_email(&pool, "owner@example.org")
        .await
        .unwrap()
        .unwrap();
    let viewer = db::user_by_email(&pool, "viewer@example.org")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        db::role_in(&pool, "cli-room", owner.id).await.unwrap(),
        Some(Role::Owner)
    );
    assert_eq!(
        db::role_in(&pool, "cli-room", viewer.id).await.unwrap(),
        Some(Role::Viewer)
    );
}

fn run_cli(db_path: &Path, args: &[&str]) {
    let output = Command::new(env!("CARGO_BIN_EXE_cocat-server"))
        .args(args)
        .arg("--db_path")
        .arg(db_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "cocat-server {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
