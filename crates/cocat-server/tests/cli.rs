use std::path::Path;
use std::process::Command;

use cocat_core::role::Role;
use cocat_server::db;
use sqlx::SqlitePool;
use tempfile::TempDir;

const STRONG_PASSWORD: &str = "plum orbit lantern cello";

async fn seeded_db() -> (TempDir, SqlitePool) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let pool = db::open(&db_path).await.unwrap();
    (dir, pool)
}

#[tokio::test]
async fn create_user_then_read_it_back() {
    let (_dir, pool) = seeded_db().await;

    let id = db::create_user(&pool, "user@example.org", "hunter2", false, true)
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
    let owner = db::create_user(&pool, "owner@example.org", "hunter2", false, true)
        .await
        .unwrap();

    db::create_room(
        &pool,
        owner,
        "room-1",
        "Room one",
        db::ProjectKind::Catalogues,
    )
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
    let owner = db::create_user(&pool, "owner@example.org", "hunter2", false, true)
        .await
        .unwrap();
    let viewer = db::create_user(&pool, "viewer@example.org", "hunter2", false, true)
        .await
        .unwrap();
    db::create_room(
        &pool,
        owner,
        "room-1",
        "Room one",
        db::ProjectKind::Catalogues,
    )
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
            STRONG_PASSWORD,
        ],
    );
    run_cli(
        &db_path,
        &[
            "create-user",
            "--email",
            "viewer@example.org",
            "--password",
            STRONG_PASSWORD,
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

    let pool = db::open(&db_path).await.unwrap();
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

#[tokio::test]
async fn creating_a_user_creates_their_personal_room() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    run_cli(
        &db_path,
        &[
            "create-user",
            "--email",
            "owner@example.org",
            "--password",
            STRONG_PASSWORD,
        ],
    );

    let pool = db::open(&db_path).await.unwrap();
    let user = db::user_by_email(&pool, "owner@example.org")
        .await
        .unwrap()
        .unwrap();
    let rooms = db::list_user_rooms(&pool, user.id).await.unwrap();

    assert_eq!(rooms.len(), 1, "the user does not own exactly one room");
    let (room, role) = &rooms[0];
    assert_eq!(room.name, "My catalogues");
    assert_eq!(*role, Role::Owner);
    assert!(
        room.id.len() == 32
            && room
                .id
                .chars()
                .all(|character| matches!(character, '0'..='9' | 'a'..='f')),
        "the personal room id is not a bare uuid: {}",
        room.id
    );
}

#[tokio::test]
async fn remove_user_from_room_drops_the_membership() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    run_cli(
        &db_path,
        &[
            "create-user",
            "--email",
            "owner@example.org",
            "--password",
            STRONG_PASSWORD,
        ],
    );
    run_cli(
        &db_path,
        &[
            "create-user",
            "--email",
            "member@example.org",
            "--password",
            STRONG_PASSWORD,
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
            "member@example.org",
            "--room_id",
            "cli-room",
            "--role",
            "editor",
        ],
    );

    run_cli(
        &db_path,
        &[
            "remove-user-from-room",
            "--email",
            "member@example.org",
            "--room_id",
            "cli-room",
        ],
    );

    let pool = db::open(&db_path).await.unwrap();
    let member = db::user_by_email(&pool, "member@example.org")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        db::role_in(&pool, "cli-room", member.id).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn set_room_owner_transfers() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    run_cli(
        &db_path,
        &[
            "create-user",
            "--email",
            "owner@example.org",
            "--password",
            STRONG_PASSWORD,
        ],
    );
    run_cli(
        &db_path,
        &[
            "create-user",
            "--email",
            "member@example.org",
            "--password",
            STRONG_PASSWORD,
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
            "member@example.org",
            "--room_id",
            "cli-room",
            "--role",
            "editor",
        ],
    );

    run_cli(
        &db_path,
        &[
            "set-room-owner",
            "--email",
            "member@example.org",
            "--room_id",
            "cli-room",
        ],
    );

    let pool = db::open(&db_path).await.unwrap();
    let owner = db::user_by_email(&pool, "owner@example.org")
        .await
        .unwrap()
        .unwrap();
    let member = db::user_by_email(&pool, "member@example.org")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        db::role_in(&pool, "cli-room", owner.id).await.unwrap(),
        Some(Role::Editor)
    );
    assert_eq!(
        db::role_in(&pool, "cli-room", member.id).await.unwrap(),
        Some(Role::Owner)
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

#[test]
fn a_malformed_budget_is_a_startup_error() {
    let dir = TempDir::new().unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cocat-server"))
        .args(["serve", "--port", "0", "--db_path"])
        .arg(dir.path().join("test.db"))
        .env("COCAT_ROOM_MEMORY_BUDGET", "many")
        .output()
        .unwrap();

    assert!(!output.status.success(), "serve accepted a bad budget");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("COCAT_ROOM_MEMORY_BUDGET"),
        "the error does not name the variable: {stderr}"
    );
}

#[tokio::test]
async fn the_cli_grants_and_revokes_superuser() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    run_cli(
        &db_path,
        &[
            "create-user",
            "--email",
            "admin@example.org",
            "--password",
            STRONG_PASSWORD,
        ],
    );

    let granted = run_cli_output(&db_path, &["set-superuser", "--email", "admin@example.org"]);
    assert!(
        granted.status.success(),
        "set-superuser failed: {}",
        String::from_utf8_lossy(&granted.stderr)
    );
    assert!(
        String::from_utf8_lossy(&granted.stdout)
            .contains("User admin@example.org is now an administrator"),
        "the grant message is wrong: {}",
        String::from_utf8_lossy(&granted.stdout)
    );
    let pool = db::open(&db_path).await.unwrap();
    let user = db::user_by_email(&pool, "admin@example.org")
        .await
        .unwrap()
        .unwrap();
    assert!(user.is_superuser, "the CLI did not grant the flag");

    let revoked = run_cli_output(
        &db_path,
        &["set-superuser", "--email", "admin@example.org", "--revoke"],
    );
    assert!(revoked.status.success());
    assert!(
        String::from_utf8_lossy(&revoked.stdout)
            .contains("User admin@example.org is no longer an administrator"),
        "the revoke message is wrong: {}",
        String::from_utf8_lossy(&revoked.stdout)
    );
    let user = db::user_by_email(&pool, "admin@example.org")
        .await
        .unwrap()
        .unwrap();
    assert!(!user.is_superuser, "the CLI did not revoke the flag");

    let unknown = run_cli_output(
        &db_path,
        &["set-superuser", "--email", "nobody@example.org"],
    );
    assert!(!unknown.status.success(), "an unknown address was accepted");
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("no user with email nobody@example.org"),
        "the error does not name the address: {}",
        String::from_utf8_lossy(&unknown.stderr)
    );
}

fn run_cli_output(db_path: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cocat-server"))
        .args(args)
        .arg("--db_path")
        .arg(db_path)
        .output()
        .unwrap()
}

#[tokio::test]
async fn create_room_refuses_an_id_kept_for_public_catalogues() {
    let (dir, pool) = seeded_db().await;
    let owner = db::create_user(&pool, "owner@example.org", "pwd", false, true)
        .await
        .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cocat-server"))
        .args(["create-room", "--owner", "owner@example.org"])
        .args(["--room-id", "public.test", "--db_path"])
        .arg(dir.path().join("test.db"))
        .output()
        .unwrap();
    let direct = db::create_room(
        &pool,
        owner,
        "public.other",
        "Other",
        db::ProjectKind::Catalogues,
    )
    .await;

    assert!(!output.status.success(), "create-room took a public id");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "\"public.test\" starts with \"public.\", which this server keeps for public catalogues"
        ),
        "{stderr}"
    );
    assert!(
        matches!(direct, Err(db::DbError::ReservedId(_))),
        "{direct:?}"
    );
    assert!(
        db::refuse_reserved_ids(&pool).await.is_ok(),
        "a room was made"
    );
}

#[test]
fn create_user_refuses_a_weak_password_and_says_why() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    let output = run_cli_output(
        &db_path,
        &[
            "create-user",
            "--email",
            "weak@example.org",
            "--password",
            "hunter2",
        ],
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Use at least 8 characters."), "{stderr}");
}
