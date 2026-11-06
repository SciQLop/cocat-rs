mod ui_support;

use std::fs;

use cocat_core::role::Role;
use cocat_server::admin::{self, AdminError};
use cocat_server::auth;
use cocat_server::db;
use ui_support::Fixture;

async fn superuser(fixture: &Fixture, email: &str) -> db::User {
    let id = db::create_user(&fixture.state.pool, email, "pwd", true, true)
        .await
        .unwrap();
    db::user_by_id(&fixture.state.pool, id)
        .await
        .unwrap()
        .expect("the account exists")
}

async fn account(fixture: &Fixture, id: uuid::Uuid) -> db::User {
    db::user_by_id(&fixture.state.pool, id)
        .await
        .unwrap()
        .expect("the account exists")
}

async fn room_with_file(fixture: &Fixture, owner: uuid::Uuid, id: &str, name: &str, bytes: usize) {
    db::create_room(&fixture.state.pool, owner, id, name)
        .await
        .unwrap();
    fs::write(
        fixture.dir().join("rooms").join(format!("{id}.y")),
        vec![7u8; bytes],
    )
    .unwrap();
}

#[tokio::test]
async fn every_admin_function_refuses_a_plain_user() {
    let fixture = Fixture::start().await;
    let plain = fixture.verified("plain@example.org").await;
    let other = fixture.verified("other@example.org").await;
    let actor = account(&fixture, plain).await;
    db::create_room(&fixture.state.pool, other, "lab", "Lab")
        .await
        .unwrap();

    assert!(matches!(
        admin::overview(&fixture.state, &actor).await,
        Err(AdminError::NotFound)
    ));
    assert!(matches!(
        admin::users(&fixture.state, &actor, None).await,
        Err(AdminError::NotFound)
    ));
    assert!(matches!(
        admin::rooms(&fixture.state, &actor).await,
        Err(AdminError::NotFound)
    ));
    assert!(matches!(
        admin::set_user_flags(&fixture.state, &actor, other, Some(false), None, None).await,
        Err(AdminError::NotFound)
    ));
    assert!(matches!(
        admin::delete_user(&fixture.state, &actor, other).await,
        Err(AdminError::NotFound)
    ));
    assert!(matches!(
        admin::set_user_quota(&fixture.state, &actor, other, Some(1)).await,
        Err(AdminError::NotFound)
    ));
    assert!(matches!(
        admin::set_room_limit(&fixture.state, &actor, "lab", Some(1)).await,
        Err(AdminError::NotFound)
    ));
    assert!(matches!(
        admin::set_room_owner(&fixture.state, &actor, "lab", "plain@example.org").await,
        Err(AdminError::NotFound)
    ));
    assert!(matches!(
        admin::delete_room(&fixture.state, &actor, "lab").await,
        Err(AdminError::NotFound)
    ));

    assert!(account(&fixture, other).await.is_active);
    assert!(
        db::room_owner(&fixture.state.pool, "lab")
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        db::user_quota(&fixture.state.pool, other).await.unwrap(),
        None
    );
    assert_eq!(
        db::room_quota(&fixture.state.pool, "lab").await.unwrap(),
        None
    );
}

#[tokio::test]
async fn deactivating_an_account_ends_its_sessions() {
    let fixture = Fixture::start().await;
    let actor = superuser(&fixture, "admin@example.org").await;
    let member = fixture.verified("member@example.org").await;
    auth::start_session(&fixture.state.pool, member)
        .await
        .unwrap();

    admin::set_user_flags(&fixture.state, &actor, member, Some(false), None, None)
        .await
        .unwrap();

    assert!(!account(&fixture, member).await.is_active);
    let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session WHERE user_id = ?")
        .bind(member.to_string())
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    assert_eq!(sessions, 0);
}

#[tokio::test]
async fn deleting_a_room_removes_its_file() {
    let fixture = Fixture::start().await;
    let actor = superuser(&fixture, "admin@example.org").await;
    let owner = fixture.verified("owner@example.org").await;
    room_with_file(&fixture, owner, "lab", "Lab", 32).await;
    let file = fixture.dir().join("rooms").join("lab.y");
    assert!(file.exists());

    admin::delete_room(&fixture.state, &actor, "lab")
        .await
        .unwrap();

    assert!(!file.exists());
    assert!(
        db::room_owner(&fixture.state.pool, "lab")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn changing_the_owner_goes_through_the_transfer() {
    let fixture = Fixture::start().await;
    let actor = superuser(&fixture, "admin@example.org").await;
    let owner = fixture.verified("owner@example.org").await;
    let member = fixture.verified("member@example.org").await;
    db::create_room(&fixture.state.pool, owner, "lab", "Lab")
        .await
        .unwrap();
    db::add_member(&fixture.state.pool, "lab", member, Role::Editor)
        .await
        .unwrap();

    admin::set_room_owner(&fixture.state, &actor, "lab", "MEMBER@example.org")
        .await
        .unwrap();

    assert_eq!(
        db::room_owner(&fixture.state.pool, "lab").await.unwrap(),
        Some(member)
    );
    assert!(matches!(
        admin::set_room_owner(&fixture.state, &actor, "lab", "ghost@example.org").await,
        Err(AdminError::UnknownUser)
    ));
}

#[tokio::test]
async fn the_admin_cannot_lock_themselves_out() {
    let fixture = Fixture::start().await;
    let actor = superuser(&fixture, "admin@example.org").await;

    assert!(matches!(
        admin::set_user_flags(&fixture.state, &actor, actor.id, Some(false), None, None).await,
        Err(AdminError::OwnAccount)
    ));
    assert!(matches!(
        admin::set_user_flags(&fixture.state, &actor, actor.id, None, None, Some(false)).await,
        Err(AdminError::OwnAccount)
    ));
    assert!(matches!(
        admin::delete_user(&fixture.state, &actor, actor.id).await,
        Err(AdminError::OwnAccount)
    ));
    assert!(account(&fixture, actor.id).await.is_active);
    assert!(account(&fixture, actor.id).await.is_superuser);
}

#[tokio::test]
async fn the_overview_counts_and_sizes_the_seeded_tenant() {
    let fixture = Fixture::start().await;
    let actor = superuser(&fixture, "admin@example.org").await;
    let owner = fixture.verified("owner@example.org").await;
    fixture.unverified("pending@example.org").await;
    room_with_file(&fixture, owner, "lab", "Lab", 400).await;
    db::create_room(&fixture.state.pool, owner, "ghost", "Ghost")
        .await
        .unwrap();

    let overview = admin::overview(&fixture.state, &actor).await.unwrap();

    assert_eq!(overview.users.total, 3);
    assert_eq!(overview.users.active, 3);
    assert_eq!(overview.users.unverified, 1);
    assert_eq!(overview.users.superusers, 1);
    assert_eq!(overview.rooms.total, 2);
    assert_eq!(overview.storage.total_bytes, 400);
    assert_eq!(overview.storage.largest.len(), 2);
    assert_eq!(overview.storage.largest[0].id, "lab");
    assert_eq!(overview.storage.largest[1].size_bytes, 0);
    assert_eq!(overview.server.version, env!("CARGO_PKG_VERSION"));
    assert!(overview.server.resident_bytes.unwrap_or(0) > 0);
}

#[tokio::test]
async fn the_user_list_filters_by_substring_ignoring_case() {
    let fixture = Fixture::start().await;
    let actor = superuser(&fixture, "admin@example.org").await;
    fixture.verified("alice@example.org").await;
    fixture.verified("bob@example.org").await;

    let found = admin::users(&fixture.state, &actor, Some("ALI"))
        .await
        .unwrap();

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].email, "alice@example.org");
    assert_eq!(
        admin::users(&fixture.state, &actor, Some("example.org"))
            .await
            .unwrap()
            .len(),
        3
    );
}

/// Deleting an account leaves the rooms it owned without an owner: the admin is who repairs
/// them, so both actions must work on an ownerless room.
#[tokio::test]
async fn an_ownerless_room_can_be_given_an_owner_or_deleted() {
    let fixture = Fixture::start().await;
    let admin = superuser(&fixture, "admin@example.org").await;
    let gone = db::create_user(&fixture.state.pool, "gone@example.org", "pwd", true, false)
        .await
        .unwrap();
    let heir = db::create_user(&fixture.state.pool, "heir@example.org", "pwd", true, false)
        .await
        .unwrap();
    room_with_file(&fixture, gone, "orphan", "Orphan", 10).await;
    room_with_file(&fixture, gone, "doomed", "Doomed", 10).await;
    admin::delete_user(&fixture.state, &admin, gone)
        .await
        .unwrap();
    assert_eq!(
        db::room_owner(&fixture.state.pool, "orphan").await.unwrap(),
        None
    );

    admin::set_room_owner(&fixture.state, &admin, "orphan", "heir@example.org")
        .await
        .expect("an ownerless room takes a new owner");
    assert_eq!(
        db::room_owner(&fixture.state.pool, "orphan").await.unwrap(),
        Some(heir)
    );

    admin::delete_room(&fixture.state, &admin, "doomed")
        .await
        .expect("an ownerless room can be deleted");
    assert!(!fixture.dir().join("rooms").join("doomed.y").exists());
}

/// The admin hands a room to anyone with an account, not only to its members.
#[tokio::test]
async fn the_new_owner_need_not_be_a_member() {
    let fixture = Fixture::start().await;
    let admin = superuser(&fixture, "admin@example.org").await;
    let owner = db::create_user(&fixture.state.pool, "owner@example.org", "pwd", true, false)
        .await
        .unwrap();
    let stranger = db::create_user(&fixture.state.pool, "new@example.org", "pwd", true, false)
        .await
        .unwrap();
    room_with_file(&fixture, owner, "lab", "Lab", 10).await;

    admin::set_room_owner(&fixture.state, &admin, "lab", "new@example.org")
        .await
        .expect("a non-member becomes the owner");

    assert_eq!(
        db::room_owner(&fixture.state.pool, "lab").await.unwrap(),
        Some(stranger)
    );
    assert_eq!(
        db::direct_role(&fixture.state.pool, "lab", owner)
            .await
            .unwrap(),
        Some(Role::Editor),
        "the previous owner stays as an editor"
    );
}
