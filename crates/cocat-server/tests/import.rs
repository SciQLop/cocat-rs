use std::path::{Path, PathBuf};

use bytes::Bytes;
use cocat_core::role::Role;
use cocat_core::store::{FileRoomStore, HEADER};
use cocat_server::room::RoomManager;
use cocat_server::{AppState, db, import, invitations, links::Links, ratelimit::LoginLimit};
use sqlx::SqlitePool;
use tempfile::TempDir;
use time::OffsetDateTime;
use time::macros::format_description;
use uuid::Uuid;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/python");

struct Fixture {
    dir: TempDir,
    pool: SqlitePool,
    updates: PathBuf,
}

impl Fixture {
    async fn start() -> Self {
        let dir = TempDir::new().unwrap();
        let updates = dir.path().join("updates");
        std::fs::create_dir_all(&updates).unwrap();
        let pool = db::open(&dir.path().join("rust.db")).await.unwrap();
        Self { dir, pool, updates }
    }

    fn db_path(&self) -> PathBuf {
        self.dir.path().join("rust.db")
    }

    fn released() -> PathBuf {
        Path::new(FIXTURES).join("released.db")
    }

    fn rooms_model() -> PathBuf {
        Path::new(FIXTURES).join("rooms_model.db")
    }

    fn state(&self) -> AppState {
        AppState {
            rooms: RoomManager::new(self.updates.clone(), None),
            pool: self.pool.clone(),
            secret: "secret".to_owned(),
            before_attach: None,
            login_limit: LoginLimit::default(),
            mailer: None,
            links: Links::default(),
            oidc: Vec::new(),
            web_dir: None,
        }
    }

    async fn user(&self, email: &str) -> db::User {
        db::user_by_email(&self.pool, email)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{email} was imported"))
    }

    async fn role(&self, room: &str, email: &str) -> Option<Role> {
        let user = self.user(email).await;
        db::role_in(&self.pool, room, user.id).await.unwrap()
    }
}

#[tokio::test]
async fn imports_released_schema() {
    let fixture = Fixture::start().await;

    let summary = import::run(
        &fixture.updates,
        &fixture.db_path(),
        Some(&Fixture::released()),
        None,
        false,
    )
    .await
    .unwrap();

    assert_eq!(summary.users, 3);
    assert_eq!(summary.rooms, 3);
    assert_eq!(summary.memberships, 3);
    assert!(fixture.user("alice@example.org").await.is_verified);
    assert_eq!(
        fixture.role("lab", "alice@example.org").await,
        Some(Role::Owner)
    );
    assert_eq!(
        fixture.role("lab", "bob@example.org").await,
        Some(Role::Editor)
    );
    assert_eq!(
        fixture.role("solo", "alice@example.org").await,
        Some(Role::Owner)
    );
}

#[tokio::test]
async fn imports_rooms_model() {
    let fixture = Fixture::start().await;

    let summary = import::run(
        &fixture.updates,
        &fixture.db_path(),
        Some(&Fixture::rooms_model()),
        None,
        false,
    )
    .await
    .unwrap();

    assert_eq!(summary.rooms, 1);
    assert_eq!(summary.memberships, 2);
    assert_eq!(summary.invitations, 1);
    assert_eq!(summary.links, 1);
    assert_eq!(
        fixture.role("lab2", "alice@example.org").await,
        Some(Role::Owner)
    );
    assert_eq!(
        fixture.role("lab2", "bob@example.org").await,
        Some(Role::Editor)
    );
    let alice = fixture.user("alice@example.org").await;
    assert_eq!(
        cocat_server::oidc::linked_providers(&fixture.pool, alice.id)
            .await
            .unwrap(),
        vec!["orcid".to_owned()]
    );
}

#[tokio::test]
async fn python_invitation_link_accepts_on_rust() {
    let fixture = Fixture::start().await;
    import::run(
        &fixture.updates,
        &fixture.db_path(),
        Some(&Fixture::rooms_model()),
        None,
        false,
    )
    .await
    .unwrap();
    fixture.user("bob@example.org").await;

    let carol = db::create_user(&fixture.pool, "carol@example.org", "hunter2", false, true)
        .await
        .unwrap();
    let carol_user = db::user_by_id(&fixture.pool, carol).await.unwrap().unwrap();

    let (room, role) =
        invitations::accept(&fixture.state(), "python-invitation-token", &carol_user)
            .await
            .unwrap();

    assert_eq!(room.id, "lab2");
    assert_eq!(role, Role::Viewer);
}

#[tokio::test]
async fn rooms_from_files_only_with_owner() {
    let fixture = Fixture::start().await;
    let owner = db::create_user(&fixture.pool, "owner@example.org", "hunter2", false, true)
        .await
        .unwrap();
    let id = Uuid::new_v4().to_string();
    let (mut store, _records) = FileRoomStore::open(&fixture.updates, &id).unwrap();
    store
        .append(Bytes::from_static(b"a room file the import only reads"))
        .unwrap();
    store.write().unwrap();
    drop(store);

    let summary = import::run(
        &fixture.updates,
        &fixture.db_path(),
        None,
        Some("owner@example.org"),
        false,
    )
    .await
    .unwrap();

    assert_eq!(summary.rooms, 1);
    assert_eq!(
        db::role_in(&fixture.pool, &id, owner).await.unwrap(),
        Some(Role::Owner)
    );
}

#[tokio::test]
async fn no_users_no_owner_refused() {
    let fixture = Fixture::start().await;

    let error = import::run(&fixture.updates, &fixture.db_path(), None, None, false)
        .await
        .unwrap_err();

    assert!(matches!(error, import::ImportError::NoOwner));
}

#[tokio::test]
async fn second_run_changes_nothing() {
    let fixture = Fixture::start().await;
    let run = || async {
        import::run(
            &fixture.updates,
            &fixture.db_path(),
            Some(&Fixture::released()),
            None,
            false,
        )
        .await
        .unwrap()
    };

    run().await;
    let before = db::list_user_rooms(&fixture.pool, fixture.user("alice@example.org").await.id)
        .await
        .unwrap()
        .len();
    run().await;

    assert_eq!(
        db::list_user_rooms(&fixture.pool, fixture.user("alice@example.org").await.id)
            .await
            .unwrap()
            .len(),
        before
    );
    assert_eq!(
        fixture.role("lab", "alice@example.org").await,
        Some(Role::Owner)
    );
}

#[tokio::test]
async fn email_conflict_stops_before_writing() {
    let fixture = Fixture::start().await;
    db::create_user(&fixture.pool, "bob@example.org", "hunter2", false, true)
        .await
        .unwrap();

    let error = import::run(
        &fixture.updates,
        &fixture.db_path(),
        Some(&Fixture::released()),
        None,
        false,
    )
    .await
    .unwrap_err();

    assert!(matches!(error, import::ImportError::Conflicts(_)));
    assert!(
        db::user_by_email(&fixture.pool, "alice@example.org")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn bad_rooms_value_skipped() {
    let fixture = Fixture::start().await;

    let summary = import::run(
        &fixture.updates,
        &fixture.db_path(),
        Some(&Fixture::released()),
        None,
        false,
    )
    .await
    .unwrap();

    assert!(
        summary
            .skipped
            .iter()
            .any(|line| line.contains("carol@example.org"))
    );
    assert!(
        db::user_by_email(&fixture.pool, "alice@example.org")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn bad_room_file_skipped_untouched() {
    let fixture = Fixture::start().await;
    let owner = db::create_user(&fixture.pool, "owner@example.org", "hunter2", false, true)
        .await
        .unwrap();
    let id = Uuid::new_v4().to_string();
    let path = fixture.updates.join(format!("{id}.y"));
    std::fs::write(&path, b"not a room file").unwrap();

    let summary = import::run(
        &fixture.updates,
        &fixture.db_path(),
        None,
        Some("owner@example.org"),
        false,
    )
    .await
    .unwrap();

    assert_eq!(summary.rooms, 0);
    assert!(summary.skipped.iter().any(|line| line.contains(&id)));
    assert_eq!(std::fs::read(&path).unwrap(), b"not a room file");
    assert!(
        db::role_in(&fixture.pool, &id, owner)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn dry_run_writes_nothing() {
    let fixture = Fixture::start().await;

    let summary = import::run(
        &fixture.updates,
        &fixture.db_path(),
        Some(&Fixture::released()),
        None,
        true,
    )
    .await
    .unwrap();

    assert_eq!(summary.users, 3);
    assert!(
        db::user_by_email(&fixture.pool, "alice@example.org")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn same_file_refused() {
    let fixture = Fixture::start().await;

    let error = import::run(
        &fixture.updates,
        &fixture.db_path(),
        Some(&fixture.db_path()),
        None,
        false,
    )
    .await
    .unwrap_err();

    assert!(matches!(error, import::ImportError::SameFile));
}

#[tokio::test]
async fn python_db_opened_read_only() {
    let fixture = Fixture::start().await;
    let python = Fixture::released();
    let before = std::fs::read(&python).unwrap();
    let modified = std::fs::metadata(&python).unwrap().modified().unwrap();

    import::run(
        &fixture.updates,
        &fixture.db_path(),
        Some(&python),
        None,
        false,
    )
    .await
    .unwrap();

    assert_eq!(std::fs::read(&python).unwrap(), before);
    assert_eq!(
        std::fs::metadata(&python).unwrap().modified().unwrap(),
        modified
    );
}

#[tokio::test]
async fn a_named_room_file_imports() {
    let fixture = Fixture::start().await;
    let owner = db::create_user(&fixture.pool, "owner@example.org", "hunter2", false, true)
        .await
        .unwrap();
    let (mut store, _records) = FileRoomStore::open(&fixture.updates, "lab").unwrap();
    store.append(Bytes::from_static(b"a room file")).unwrap();
    store.write().unwrap();
    drop(store);

    let summary = import::run(
        &fixture.updates,
        &fixture.db_path(),
        None,
        Some("owner@example.org"),
        false,
    )
    .await
    .unwrap();

    assert_eq!(summary.rooms, 1);
    assert_eq!(
        db::role_in(&fixture.pool, "lab", owner).await.unwrap(),
        Some(Role::Owner)
    );
}

#[tokio::test]
async fn python_room_names_import_and_open() {
    let fixture = Fixture::start().await;
    let owner = db::create_user(&fixture.pool, "owner@example.org", "hunter2", false, true)
        .await
        .unwrap();
    let ids = ["Bepi MSA", "alexis.jeandet"];
    for id in ids {
        std::fs::write(fixture.updates.join(format!("{id}.y")), HEADER).unwrap();
    }

    let summary = import::run(
        &fixture.updates,
        &fixture.db_path(),
        None,
        Some("owner@example.org"),
        false,
    )
    .await
    .unwrap();

    assert_eq!(summary.rooms, ids.len(), "{:?}", summary.skipped);
    let state = fixture.state();
    for id in ids {
        assert_eq!(
            db::role_in(&fixture.pool, id, owner).await.unwrap(),
            Some(Role::Owner)
        );
        state.rooms.get_or_open(id).unwrap();
    }
}

#[tokio::test]
async fn an_invitation_expiring_soon_is_accepted() {
    let fixture = Fixture::start().await;
    let python = fixture.dir.path().join("soon.db");
    std::fs::copy(Fixture::rooms_model(), &python).unwrap();
    let pool = SqlitePool::connect(&format!("sqlite://{}", python.display()))
        .await
        .unwrap();
    let soon = OffsetDateTime::now_utc() + time::Duration::hours(1);
    let text = soon
        .format(format_description!(
            "[year]-[month]-[day] [hour]:[minute]:[second].[subsecond]"
        ))
        .unwrap();
    sqlx::query("UPDATE invitation SET expires_at = ?")
        .bind(text)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    import::run(
        &fixture.updates,
        &fixture.db_path(),
        Some(&python),
        None,
        false,
    )
    .await
    .unwrap();
    let carol = db::create_user(&fixture.pool, "carol@example.org", "hunter2", false, true)
        .await
        .unwrap();
    let carol_user = db::user_by_id(&fixture.pool, carol).await.unwrap().unwrap();

    let (room, _role) =
        invitations::accept(&fixture.state(), "python-invitation-token", &carol_user)
            .await
            .expect("an invitation that expires in an hour is still usable");

    assert_eq!(room.id, "lab2");
}

/// A room Python cocat wrote, in layout v1.
const V1_ROOM: &[u8] = include_bytes!("../../cocat-core/tests/fixtures/catalogues.y");

fn room_document(updates: &Path, room_id: &str) -> yrs::Doc {
    let doc = yrs::Doc::new();
    for update in cocat_core::store::read_updates(updates, room_id).unwrap() {
        let update = <yrs::Update as yrs::updates::decoder::Decode>::decode_v1(&update).unwrap();
        yrs::Transact::transact_mut(&doc)
            .apply_update(update)
            .unwrap();
    }
    doc
}

#[tokio::test]
async fn an_imported_v1_room_file_is_migrated_to_v2() {
    let fixture = Fixture::start().await;
    db::create_user(&fixture.pool, "owner@example.org", "hunter2", false, true)
        .await
        .unwrap();
    std::fs::write(fixture.updates.join("lab.y"), V1_ROOM).unwrap();

    let summary = import::run(
        &fixture.updates,
        &fixture.db_path(),
        None,
        Some("owner@example.org"),
        false,
    )
    .await
    .unwrap();

    assert_eq!(summary.migrated, 1, "{:?}", summary.lines(false));
    let doc = room_document(&fixture.updates, "lab");
    let txn = yrs::Transact::transact(&doc);
    assert!(!cocat_core::model::is_v1(&txn));
    let names: Vec<String> = cocat_core::model::catalogues(&txn)
        .into_iter()
        .map(|catalogue| catalogue.name)
        .collect();
    assert_eq!(names, ["alpha", "beta"]);
}

#[tokio::test]
async fn a_dry_run_leaves_a_v1_room_file_alone() {
    let fixture = Fixture::start().await;
    db::create_user(&fixture.pool, "owner@example.org", "hunter2", false, true)
        .await
        .unwrap();
    let path = fixture.updates.join("lab.y");
    std::fs::write(&path, V1_ROOM).unwrap();

    let summary = import::run(
        &fixture.updates,
        &fixture.db_path(),
        None,
        Some("owner@example.org"),
        true,
    )
    .await
    .unwrap();

    assert_eq!(summary.migrated, 1);
    assert_eq!(std::fs::read(&path).unwrap(), V1_ROOM);
}

#[tokio::test]
async fn an_event_without_times_is_dropped_and_named_in_the_summary() {
    let fixture = Fixture::start().await;
    db::create_user(&fixture.pool, "owner@example.org", "hunter2", false, true)
        .await
        .unwrap();
    std::fs::write(fixture.updates.join("lab.y"), V1_ROOM).unwrap();

    let summary = import::run(
        &fixture.updates,
        &fixture.db_path(),
        None,
        Some("owner@example.org"),
        false,
    )
    .await
    .unwrap();

    let malformed = "11111111-2222-4333-8444-555555555555";
    assert!(
        summary
            .lines(false)
            .iter()
            .any(|line| line.contains(malformed) && line.contains("lab")),
        "{:?}",
        summary.lines(false)
    );
}

#[tokio::test]
async fn the_room_files_of_imported_users_are_migrated_too() {
    let fixture = Fixture::start().await;
    std::fs::write(fixture.updates.join("lab.y"), V1_ROOM).unwrap();

    let summary = import::run(
        &fixture.updates,
        &fixture.db_path(),
        Some(&Fixture::released()),
        None,
        false,
    )
    .await
    .unwrap();

    assert_eq!(summary.migrated, 1);
    let doc = room_document(&fixture.updates, "lab");
    assert!(!cocat_core::model::is_v1(&yrs::Transact::transact(&doc)));
}
