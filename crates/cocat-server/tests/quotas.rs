mod ui_support;

use cocat_server::admin::{self, AdminError};
use cocat_server::db;
use cocat_server::room::StorageLimits;
use ui_support::Fixture;

const MIB: u64 = 1024 * 1024;

async fn superuser(fixture: &Fixture, email: &str) -> db::User {
    let id = db::create_user(&fixture.state.pool, email, "pwd", true, true)
        .await
        .unwrap();
    db::user_by_id(&fixture.state.pool, id)
        .await
        .unwrap()
        .expect("the account exists")
}

#[tokio::test]
async fn a_room_limit_override_caps_the_allowance() {
    let fixture = Fixture::start().await;
    let owner = fixture.verified("owner@example.org").await;
    db::create_room(
        &fixture.state.pool,
        owner,
        "lab",
        "Lab",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    let StorageLimits {
        room, owner: quota, ..
    } = fixture.state.rooms.limits();
    assert_eq!(
        fixture
            .state
            .rooms
            .allowance(&fixture.state.pool, "lab")
            .await
            .unwrap(),
        room.min(quota)
    );
    db::set_room_quota(&fixture.state.pool, "lab", Some(3))
        .await
        .unwrap();
    assert_eq!(
        fixture
            .state
            .rooms
            .allowance(&fixture.state.pool, "lab")
            .await
            .unwrap(),
        3 * MIB
    );
}

#[tokio::test]
async fn an_owner_quota_override_caps_the_allowance() {
    let fixture = Fixture::start().await;
    let owner = fixture.verified("owner@example.org").await;
    db::create_room(
        &fixture.state.pool,
        owner,
        "lab",
        "Lab",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    db::set_user_quota(&fixture.state.pool, owner, Some(4))
        .await
        .unwrap();
    assert_eq!(
        fixture
            .state
            .rooms
            .allowance(&fixture.state.pool, "lab")
            .await
            .unwrap(),
        4 * MIB
    );
}

#[tokio::test]
async fn clearing_an_override_returns_to_the_configured_default() {
    let fixture = Fixture::start().await;
    let owner = fixture.verified("owner@example.org").await;
    db::create_room(
        &fixture.state.pool,
        owner,
        "lab",
        "Lab",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    let StorageLimits {
        room, owner: quota, ..
    } = fixture.state.rooms.limits();
    let default = fixture
        .state
        .rooms
        .allowance(&fixture.state.pool, "lab")
        .await
        .unwrap();
    for (setting, expected) in [(Some(2), 2 * MIB), (None, default)] {
        db::set_room_quota(&fixture.state.pool, "lab", setting)
            .await
            .unwrap();
        db::set_user_quota(&fixture.state.pool, owner, setting)
            .await
            .unwrap();
        let allowance = fixture
            .state
            .rooms
            .allowance(&fixture.state.pool, "lab")
            .await
            .unwrap();
        assert_eq!(allowance, expected.min(room).min(quota));
    }
}

#[tokio::test]
async fn a_zero_quota_is_refused() {
    let fixture = Fixture::start().await;
    let actor = superuser(&fixture, "admin@example.org").await;
    let owner = fixture.verified("owner@example.org").await;
    db::create_room(
        &fixture.state.pool,
        owner,
        "lab",
        "Lab",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    assert!(matches!(
        admin::set_room_limit(&fixture.state, &actor, "lab", Some(0)).await,
        Err(AdminError::InvalidQuota)
    ));
    assert!(matches!(
        admin::set_user_quota(&fixture.state, &actor, owner, Some(0)).await,
        Err(AdminError::InvalidQuota)
    ));
    assert_eq!(
        db::room_quota(&fixture.state.pool, "lab").await.unwrap(),
        None
    );
    assert_eq!(
        db::user_quota(&fixture.state.pool, owner).await.unwrap(),
        None
    );
}

fn big_update(bytes: usize) -> Vec<u8> {
    use yrs::{Doc, Map, ReadTxn, StateVector, Transact};
    let doc = Doc::new();
    doc.get_or_insert_map("content")
        .insert(&mut doc.transact_mut(), "text", "x".repeat(bytes));
    doc.transact()
        .encode_state_as_update_v1(&StateVector::default())
}

#[tokio::test]
async fn a_workspace_documents_count_toward_the_owner_quota() {
    use cocat_server::documents;
    use cocat_server::room::{Author, RoomError};
    let fixture = Fixture::start().await;
    let rooms = &fixture.state.rooms;
    let pool = &fixture.state.pool;
    let owner = fixture.verified("owner@example.org").await;
    db::create_room(pool, owner, "lab", "Lab", db::ProjectKind::Workspace)
        .await
        .unwrap();
    db::set_user_quota(pool, owner, Some(1)).await.unwrap();
    let mut keys = Vec::new();
    for _ in 0..2 {
        let doc = db::create_document(pool, "lab", db::DocumentKind::Notebook, owner)
            .await
            .unwrap();
        keys.push(documents::key("lab", doc));
    }
    let write = |key: String| async move {
        let allowance = rooms.allowance(pool, &key).await.unwrap();
        rooms
            .write_as(&key, allowance, Author::Client(owner), big_update(600_000))
            .await
    };

    let first = write(keys[0].clone()).await;
    let second = write(keys[1].clone()).await;

    assert!(first.unwrap());
    assert!(matches!(second, Err(RoomError::Full)));
}
