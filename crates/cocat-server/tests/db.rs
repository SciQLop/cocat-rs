use cocat_core::role::Role;
use cocat_server::db;
use sqlx::{Row, SqlitePool};
use tempfile::TempDir;
use tokio::time::timeout;
use uuid::Uuid;

async fn seeded() -> (TempDir, SqlitePool) {
    let dir = TempDir::new().unwrap();
    let pool = db::open(&dir.path().join("test.db")).await.unwrap();
    (dir, pool)
}

async fn user(pool: &SqlitePool, email: &str) -> Uuid {
    db::create_user(pool, email, "hunter2", false, true)
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
    let first_pool = db::open(&path).await.unwrap();
    let first: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&first_pool)
        .await
        .unwrap();
    first_pool.close().await;

    let pool = db::open(&path).await.unwrap();

    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(first >= 1, "the first open must apply the migrations");
    assert_eq!(
        applied, first,
        "the second open must not run a migration twice"
    );
    let owner = user(&pool, "owner@example.org").await;
    db::create_room(
        &pool,
        owner,
        "room-1",
        "Room one",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_second_owner_is_rejected_by_the_schema() {
    let (_dir, pool) = seeded().await;
    let owner = user(&pool, "owner@example.org").await;
    let other = user(&pool, "other@example.org").await;
    db::create_room(
        &pool,
        owner,
        "room-1",
        "Room one",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();

    let promoted = db::add_member(&pool, "room-1", other, Role::Owner).await;

    assert!(promoted.is_err(), "two owners must be impossible");
    assert_eq!(role(&pool, "room-1", owner).await, Some(Role::Owner));
    assert_eq!(role(&pool, "room-1", other).await, None);
}

#[tokio::test]
async fn add_member_never_lowers_a_role() {
    let (_dir, pool) = seeded().await;
    let owner = user(&pool, "owner@example.org").await;
    let other = user(&pool, "other@example.org").await;
    db::create_room(
        &pool,
        owner,
        "room-1",
        "Room one",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();

    let kept = db::add_member(&pool, "room-1", owner, Role::Editor)
        .await
        .unwrap();
    let editor = db::add_member(&pool, "room-1", other, Role::Editor)
        .await
        .unwrap();
    let kept_viewer = db::add_member(&pool, "room-1", other, Role::Viewer)
        .await
        .unwrap();

    assert_eq!(kept, Role::Owner, "an owner stays the owner");
    assert_eq!(editor, Role::Editor);
    assert_eq!(kept_viewer, Role::Editor, "an editor stays an editor");
    assert_eq!(role(&pool, "room-1", owner).await, Some(Role::Owner));
    assert_eq!(role(&pool, "room-1", other).await, Some(Role::Editor));
}

#[tokio::test]
async fn add_member_raises_a_role() {
    let (_dir, pool) = seeded().await;
    let owner = user(&pool, "owner@example.org").await;
    let other = user(&pool, "other@example.org").await;
    db::create_room(
        &pool,
        owner,
        "room-1",
        "Room one",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();

    let viewer = db::add_member(&pool, "room-1", other, Role::Viewer)
        .await
        .unwrap();
    let editor = db::add_member(&pool, "room-1", other, Role::Editor)
        .await
        .unwrap();

    assert_eq!(viewer, Role::Viewer);
    assert_eq!(editor, Role::Editor);
    assert_eq!(role(&pool, "room-1", other).await, Some(Role::Editor));
}

#[tokio::test]
async fn set_owner_moves_the_owner_role() {
    let (_dir, pool) = seeded().await;
    let owner = user(&pool, "owner@example.org").await;
    let other = user(&pool, "other@example.org").await;
    db::create_room(
        &pool,
        owner,
        "room-1",
        "Room one",
        db::ProjectKind::Catalogues,
    )
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
    let id = db::create_user(&pool, "Alice@X.org", "hunter2", false, true)
        .await
        .unwrap();

    let found = db::user_by_email(&pool, "alice@x.org").await.unwrap();
    let again = db::create_user(&pool, "alice@x.org", "hunter2", false, true).await;

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
    db::create_room(
        &pool,
        owner,
        "room-1",
        "Room one",
        db::ProjectKind::Catalogues,
    )
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

#[tokio::test]
async fn an_immediate_transaction_waits_for_the_writer() {
    let (_dir, pool) = seeded().await;
    let mut held = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    sqlx::query("INSERT INTO user_group (id, name) VALUES ('held', 'Held')")
        .execute(&mut *held)
        .await
        .unwrap();

    let blocked = timeout(
        std::time::Duration::from_millis(200),
        db::begin_immediate(&pool),
    )
    .await;
    assert!(
        !matches!(blocked, Ok(Ok(_))),
        "an immediate transaction must wait for the writer that holds the lock"
    );

    held.rollback().await.unwrap();
    let mut late = db::begin_immediate(&pool).await.unwrap();
    sqlx::query("INSERT INTO user_group (id, name) VALUES ('late', 'Late')")
        .execute(&mut *late)
        .await
        .unwrap();
    late.commit().await.unwrap();
    let groups: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_group")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        groups, 1,
        "the held write rolled back and the immediate one committed"
    );
}

/// A database as the server left it before migration `version`.
async fn before_migration(path: &std::path::Path, version: i64) -> SqlitePool {
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.migrations = migrator
        .migrations
        .iter()
        .filter(|migration| migration.version < version)
        .cloned()
        .collect::<Vec<_>>()
        .into();
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePool::connect_with(options).await.unwrap();
    migrator.run(&pool).await.unwrap();
    pool
}

async fn python_room(pool: &SqlitePool, id: &str, name: &str, created: &str, owner: &str) {
    sqlx::query("INSERT INTO room (id, name, created_at, created_by) VALUES (?, ?, ?, ?)")
        .bind(id)
        .bind(name)
        .bind(created)
        .bind(owner)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO membership (room_id, user_id, role) VALUES (?, ?, 'owner')")
        .bind(id)
        .bind(owner)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn migration_10_marks_the_own_project_of_imported_accounts() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.db");
    let old = before_migration(&path, 10).await;
    let mut people = Vec::new();
    for email in ["alice@example.org", "bob@example.org", "carol@example.org"] {
        people.push(user(&old, email).await.to_string());
    }
    let (alice, bob, carol) = (&people[0], &people[1], &people[2]);
    let newer = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let older = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab";
    python_room(&old, newer, "Personal", "2026-01-02 00:00:00", alice).await;
    python_room(&old, older, "Personal", "2026-01-01 00:00:00", alice).await;
    python_room(&old, "lab", "Personal", "2025-01-01 00:00:00", alice).await;
    python_room(&old, "bob-personal", "Personal", "2025-01-01 00:00:00", bob).await;
    let carols = "cccccccccccccccccccccccccccccccc";
    python_room(&old, carols, "My catalogues", "2026-01-01 00:00:00", carol).await;
    old.close().await;

    let pool = db::open(&path).await.unwrap();

    let own = |id: &str| db::personal_room(&pool, id.parse().unwrap());
    assert_eq!(own(alice).await.unwrap().as_deref(), Some(older));
    assert_eq!(
        own(bob).await.unwrap(),
        None,
        "not a uuid: not the own project"
    );
    assert_eq!(own(carol).await.unwrap().as_deref(), Some(carols));
}

/// Python production's own room is the one named by the email's local part, owned by
/// that account.
#[tokio::test]
async fn migration_11_marks_the_room_named_by_the_email_as_the_own_project() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.db");
    let old = before_migration(&path, 11).await;
    let mut people = Vec::new();
    for email in [
        "alice.smith@example.org",
        "bob@example.org",
        "carol@example.org",
    ] {
        people.push(user(&old, email).await.to_string());
    }
    let (alice, bob, carol) = (&people[0], &people[1], &people[2]);
    python_room(&old, "alice.smith", "Alice", "2025-01-01 00:00:00", alice).await;
    python_room(&old, "bob", "Bob", "2025-01-01 00:00:00", alice).await;
    let carols = "cccccccccccccccccccccccccccccccc";
    python_room(&old, carols, "Personal", "2026-01-01 00:00:00", carol).await;
    python_room(&old, "carol", "Carol", "2025-01-01 00:00:00", carol).await;
    sqlx::query(r#"UPDATE "user" SET personal_room = ? WHERE id = ?"#)
        .bind(carols)
        .bind(carol)
        .execute(&old)
        .await
        .unwrap();
    old.close().await;

    let pool = db::open(&path).await.unwrap();

    let own = |id: &str| db::personal_room(&pool, id.parse().unwrap());
    assert_eq!(own(alice).await.unwrap().as_deref(), Some("alice.smith"));
    assert_eq!(
        own(bob).await.unwrap(),
        None,
        "the room named like bob belongs to alice"
    );
    assert_eq!(
        own(carol).await.unwrap().as_deref(),
        Some(carols),
        "an account already marked keeps its own project"
    );
}

#[tokio::test]
async fn the_own_project_is_forgotten_when_deleted_or_given_away() {
    let (_dir, pool) = seeded().await;
    let alice = user(&pool, "alice@example.org").await;
    let bob = user(&pool, "bob@example.org").await;
    db::create_personal_room(&pool, alice).await.unwrap();
    db::create_personal_room(&pool, bob).await.unwrap();
    let alices = db::personal_room(&pool, alice).await.unwrap().unwrap();
    let bobs = db::personal_room(&pool, bob).await.unwrap().unwrap();
    db::add_member(&pool, &alices, bob, Role::Editor)
        .await
        .unwrap();

    db::set_owner(&pool, &alices, bob).await.unwrap();
    db::delete_room(&pool, &bobs).await.unwrap();

    assert_eq!(db::personal_room(&pool, alice).await.unwrap(), None);
    assert_eq!(db::personal_room(&pool, bob).await.unwrap(), None);
}

#[tokio::test]
async fn a_new_project_is_a_catalogue_project_unless_asked() {
    let (_dir, pool) = seeded().await;
    let owner = user(&pool, "owner@example.org").await;
    db::create_room(&pool, owner, "cats", "Cats", db::ProjectKind::Catalogues)
        .await
        .unwrap();
    db::create_room(&pool, owner, "work", "Work", db::ProjectKind::Workspace)
        .await
        .unwrap();
    db::create_personal_room(&pool, owner).await.unwrap();
    let personal = db::personal_room(&pool, owner).await.unwrap().unwrap();

    assert_eq!(
        db::project_kind(&pool, "cats").await.unwrap(),
        Some(db::ProjectKind::Catalogues)
    );
    assert_eq!(
        db::project_kind(&pool, "work").await.unwrap(),
        Some(db::ProjectKind::Workspace)
    );
    assert_eq!(
        db::project_kind(&pool, &personal).await.unwrap(),
        Some(db::ProjectKind::Catalogues)
    );
    assert_eq!(db::project_kind(&pool, "nope").await.unwrap(), None);
}

#[tokio::test]
async fn a_document_belongs_to_its_project() {
    let (_dir, pool) = seeded().await;
    let owner = user(&pool, "owner@example.org").await;
    for id in ["one", "two"] {
        db::create_room(&pool, owner, id, id, db::ProjectKind::Workspace)
            .await
            .unwrap();
    }
    let manifest = db::create_document(&pool, "one", db::DocumentKind::Manifest, owner)
        .await
        .unwrap();
    let notebook = db::create_document(&pool, "one", db::DocumentKind::Notebook, owner)
        .await
        .unwrap();
    let elsewhere = db::create_document(&pool, "two", db::DocumentKind::Text, owner)
        .await
        .unwrap();

    let mut listed = db::documents_of(&pool, "one").await.unwrap();
    listed.sort_by_key(|(_, kind)| kind.as_str());
    assert_eq!(
        listed,
        vec![
            (manifest, db::DocumentKind::Manifest),
            (notebook, db::DocumentKind::Notebook)
        ]
    );
    assert_eq!(
        db::document_kind(&pool, "one", notebook).await.unwrap(),
        Some(db::DocumentKind::Notebook)
    );
    assert_eq!(
        db::document_kind(&pool, "one", elsewhere).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn deleting_a_project_deletes_its_documents() {
    let (_dir, pool) = seeded().await;
    let owner = user(&pool, "owner@example.org").await;
    db::create_room(&pool, owner, "one", "One", db::ProjectKind::Workspace)
        .await
        .unwrap();
    db::create_document(&pool, "one", db::DocumentKind::Text, owner)
        .await
        .unwrap();

    sqlx::query("DELETE FROM room WHERE id = 'one'")
        .execute(&pool)
        .await
        .unwrap();

    assert!(db::documents_of(&pool, "one").await.unwrap().is_empty());
}
