use std::fs;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use cocat_core::role::Role;
use cocat_server::room::{CloseReason, RoomManager, outbox};
use cocat_server::{
    AppState, accounts, auth, db, lifecycle, links::Links, ratelimit::LoginLimit, ws,
};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::time::timeout;
use tower::ServiceExt;
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(10);

struct Fixture {
    state: AppState,
    pool: SqlitePool,
    _dir: TempDir,
}

impl Fixture {
    async fn start() -> Self {
        let dir = TempDir::new().unwrap();
        let rooms = dir.path().join("rooms");
        fs::create_dir_all(&rooms).unwrap();
        let pool = db::open(&dir.path().join("test.db")).await.unwrap();
        let state = AppState {
            pool: pool.clone(),
            secret: "test-secret".to_owned(),
            rooms: RoomManager::new(rooms, None),
            before_attach: None,
            login_limit: LoginLimit::default(),
            mailer: None,
            links: Links::default(),
            oidc: Vec::new(),
            web_dir: None,
        };
        Self {
            state,
            pool,
            _dir: dir,
        }
    }

    async fn user(&self, email: &str) -> Uuid {
        db::create_user(&self.pool, email, "hunter2", false, true)
            .await
            .unwrap()
    }

    async fn room(&self, room_id: &str, owner: Uuid) {
        db::create_room(
            &self.pool,
            owner,
            room_id,
            room_id,
            db::ProjectKind::Catalogues,
        )
        .await
        .unwrap();
    }

    async fn get(&self, path: &str, user: Uuid) -> (StatusCode, serde_json::Value) {
        let token = auth::start_session(&self.state.pool, user).await.unwrap();
        let request = Request::builder()
            .method(Method::GET)
            .uri(path)
            .header(header::COOKIE, format!("fastapiusersauth={token}"))
            .body(Body::empty())
            .unwrap();
        let response = ws::router(self.state.clone())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("the response is not JSON")
        };
        (status, body)
    }
}

/// Inserts a group the way Task 2's `groups::create` will, so these tests do not depend on it.
async fn group(pool: &SqlitePool, id: &str, owner: Uuid) -> String {
    sqlx::query("INSERT INTO user_group (id, name, created_by) VALUES (?, ?, ?)")
        .bind(id)
        .bind(id)
        .bind(owner.to_string())
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO group_member (group_id, user_id, role) VALUES (?, ?, 'owner')")
        .bind(id)
        .bind(owner.to_string())
        .execute(pool)
        .await
        .unwrap();
    id.to_owned()
}

async fn join(pool: &SqlitePool, group_id: &str, user: Uuid, role: &str) {
    sqlx::query("INSERT INTO group_member (group_id, user_id, role) VALUES (?, ?, ?)")
        .bind(group_id)
        .bind(user.to_string())
        .bind(role)
        .execute(pool)
        .await
        .unwrap();
}

async fn grant(pool: &SqlitePool, room_id: &str, group_id: &str, role: &str) {
    sqlx::query(
        "INSERT INTO room_group (room_id, group_id, role) VALUES (?, ?, ?)
         ON CONFLICT (room_id, group_id) DO UPDATE SET role = excluded.role",
    )
    .bind(room_id)
    .bind(group_id)
    .bind(role)
    .execute(pool)
    .await
    .unwrap();
}

async fn role(pool: &SqlitePool, room_id: &str, user: Uuid) -> Option<Role> {
    db::role_in(pool, room_id, user).await.unwrap()
}

#[tokio::test]
async fn group_grant_gives_access() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let guest = fixture.user("guest@example.org").await;
    fixture.room("room-shared", owner).await;
    let shared = group(&fixture.pool, "group-1", owner).await;
    join(&fixture.pool, &shared, guest, "member").await;
    grant(&fixture.pool, "room-shared", &shared, "viewer").await;

    assert_eq!(
        role(&fixture.pool, "room-shared", guest).await,
        Some(Role::Viewer)
    );
    assert_eq!(
        db::direct_role(&fixture.pool, "room-shared", guest)
            .await
            .unwrap(),
        None,
        "the guest has no membership of their own"
    );
}

#[tokio::test]
async fn highest_role_wins() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let viewer = fixture.user("viewer@example.org").await;
    let editor = fixture.user("editor@example.org").await;
    fixture.room("room-raised", owner).await;
    let raising = group(&fixture.pool, "group-raising", owner).await;
    let lowering = group(&fixture.pool, "group-lowering", owner).await;
    let other = group(&fixture.pool, "group-other", owner).await;

    db::add_member(&fixture.pool, "room-raised", viewer, Role::Viewer)
        .await
        .unwrap();
    join(&fixture.pool, &raising, viewer, "member").await;
    grant(&fixture.pool, "room-raised", &raising, "editor").await;
    assert_eq!(
        role(&fixture.pool, "room-raised", viewer).await,
        Some(Role::Editor),
        "a group grant raises a direct viewer"
    );

    db::add_member(&fixture.pool, "room-raised", editor, Role::Editor)
        .await
        .unwrap();
    join(&fixture.pool, &lowering, editor, "member").await;
    grant(&fixture.pool, "room-raised", &lowering, "viewer").await;
    assert_eq!(
        role(&fixture.pool, "room-raised", editor).await,
        Some(Role::Editor),
        "a lower group grant does not lower a direct editor"
    );

    grant(&fixture.pool, "room-raised", &other, "editor").await;
    assert_eq!(
        role(&fixture.pool, "room-raised", owner).await,
        Some(Role::Owner),
        "the owner stays the owner"
    );
}

#[tokio::test]
async fn rooms_through_groups_are_listed_once() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let member = fixture.user("member@example.org").await;
    fixture.room("room-once", owner).await;
    let editors = group(&fixture.pool, "group-editors", owner).await;
    let viewers = group(&fixture.pool, "group-viewers", owner).await;

    db::add_member(&fixture.pool, "room-once", member, Role::Viewer)
        .await
        .unwrap();
    join(&fixture.pool, &editors, member, "member").await;
    grant(&fixture.pool, "room-once", &editors, "editor").await;
    join(&fixture.pool, &viewers, member, "member").await;
    grant(&fixture.pool, "room-once", &viewers, "viewer").await;

    let rooms = db::list_user_rooms(&fixture.pool, member).await.unwrap();

    assert_eq!(rooms.len(), 1, "one row per room, not one per grant");
    assert_eq!(rooms[0].0.id, "room-once");
    assert_eq!(rooms[0].1, Role::Editor);
}

#[tokio::test]
async fn group_delete_cascades_grants() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let member = fixture.user("member@example.org").await;
    fixture.room("room-cascade", owner).await;
    let cascaded = group(&fixture.pool, "group-cascade", owner).await;
    db::add_member(&fixture.pool, "room-cascade", member, Role::Viewer)
        .await
        .unwrap();
    join(&fixture.pool, &cascaded, member, "member").await;
    grant(&fixture.pool, "room-cascade", &cascaded, "editor").await;

    sqlx::query("DELETE FROM user_group WHERE id = ?")
        .bind(&cascaded)
        .execute(&fixture.pool)
        .await
        .unwrap();

    let grants: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM room_group")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    let members: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM group_member WHERE group_id = ?")
        .bind(&cascaded)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(grants, 0, "the grant went with the group");
    assert_eq!(members, 0, "the memberships went with the group");
    assert_eq!(
        role(&fixture.pool, "room-cascade", member).await,
        Some(Role::Viewer),
        "the direct membership still stands"
    );
}

#[tokio::test]
async fn removing_a_direct_member_keeps_group_access() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let member = fixture.user("member@example.org").await;
    fixture.room("room-keep", owner).await;
    db::add_member(&fixture.pool, "room-keep", member, Role::Viewer)
        .await
        .unwrap();
    let kept = group(&fixture.pool, "group-keep", owner).await;
    join(&fixture.pool, &kept, member, "member").await;
    grant(&fixture.pool, "room-keep", &kept, "editor").await;

    lifecycle::remove_member(&fixture.state, "room-keep", owner, member)
        .await
        .unwrap();

    assert_eq!(
        db::direct_role(&fixture.pool, "room-keep", member)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        role(&fixture.pool, "room-keep", member).await,
        Some(Role::Editor),
        "the group still gives the room"
    );

    let (status, body) = fixture.get("/api/rooms", member).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["id"], "room-keep");
    assert_eq!(body[0]["role"], "editor");
}

#[tokio::test]
async fn deleting_a_user_revokes_group_rooms() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let member = fixture.user("member@example.org").await;
    fixture.room("room-revoke", owner).await;
    let revoked = group(&fixture.pool, "group-revoke", owner).await;
    join(&fixture.pool, &revoked, member, "member").await;
    grant(&fixture.pool, "room-revoke", &revoked, "editor").await;

    let room = fixture.state.rooms.get_or_open("room-revoke").unwrap();
    let (out, mut inbox) = outbox();
    room.attach(1, member, "test@example.org".to_owned(), Role::Editor, out)
        .unwrap();

    assert!(accounts::delete(&fixture.state, member).await.unwrap());

    let closed = timeout(WAIT, &mut inbox.close)
        .await
        .expect("the room closed the socket of the deleted account");
    assert_eq!(closed.ok(), Some(CloseReason::SessionEnded));
}

#[tokio::test]
async fn a_room_joined_just_before_the_account_delete_is_revoked() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let member = fixture.user("member@example.org").await;
    fixture.room("room-late", owner).await;

    let room = fixture.state.rooms.get_or_open("room-late").unwrap();
    let (out, mut inbox) = outbox();
    room.attach(1, member, "test@example.org".to_owned(), Role::Editor, out)
        .unwrap();

    db::add_member(&fixture.pool, "room-late", member, Role::Editor)
        .await
        .unwrap();

    assert!(accounts::delete(&fixture.state, member).await.unwrap());

    let closed = timeout(WAIT, &mut inbox.close)
        .await
        .expect("the delete must revoke a room it found in the same transaction");
    assert_eq!(closed.ok(), Some(CloseReason::SessionEnded));
}
