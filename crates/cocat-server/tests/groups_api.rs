use std::fs;
use std::time::{Duration, SystemTime};

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use cocat_core::role::Role;
use cocat_server::room::{CloseReason, RoomManager, outbox};
use cocat_server::{AppState, auth, db, links::Links, ratelimit::LoginLimit, ws};
use serde_json::{Value, json};
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
        db::create_room(&self.pool, owner, room_id, room_id)
            .await
            .unwrap();
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        user: Option<Uuid>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(user) = user {
            let token = auth::mint_token(user, &self.state.secret, SystemTime::now()).unwrap();
            request = request.header(header::COOKIE, format!("fastapiusersauth={token}"));
        }
        let body = match body {
            Some(body) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(serde_json::to_vec(&body).unwrap())
            }
            None => Body::empty(),
        };
        let response = ws::router(self.state.clone())
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("the response is not JSON")
        };
        (status, body)
    }

    async fn get(&self, path: &str, user: Option<Uuid>) -> (StatusCode, Value) {
        self.send(Method::GET, path, user, None).await
    }

    async fn post(&self, path: &str, user: Uuid, body: Value) -> (StatusCode, Value) {
        self.send(Method::POST, path, Some(user), Some(body)).await
    }

    async fn patch(&self, path: &str, user: Uuid, body: Value) -> (StatusCode, Value) {
        self.send(Method::PATCH, path, Some(user), Some(body)).await
    }

    async fn delete(&self, path: &str, user: Uuid) -> (StatusCode, Value) {
        self.send(Method::DELETE, path, Some(user), None).await
    }
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
    sqlx::query("INSERT INTO room_group (room_id, group_id, role) VALUES (?, ?, ?)")
        .bind(room_id)
        .bind(group_id)
        .bind(role)
        .execute(pool)
        .await
        .unwrap();
}

async fn members_of(pool: &SqlitePool, group_id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM group_member WHERE group_id = ?")
        .bind(group_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Creates a group through the API and returns its id.
async fn create(fixture: &Fixture, owner: Uuid, name: &str) -> String {
    let (status, body) = fixture
        .post("/api/groups", owner, json!({ "name": name }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn create_and_list() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;

    let (status, body) = fixture
        .post("/api/groups", alice, json!({ "name": "Team" }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["name"], "Team");
    assert_eq!(body["role"], "owner");
    let id = body["id"].as_str().unwrap().to_owned();

    let (status, body) = fixture.get("/api/groups", Some(alice)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "groups": [{ "id": id, "name": "Team", "role": "owner" }] })
    );
}

#[tokio::test]
async fn name_length_is_422() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;

    for name in ["", &"x".repeat(201)] {
        let (status, _) = fixture
            .post("/api/groups", alice, json!({ "name": name }))
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{name:?}");
    }

    let (status, _) = fixture
        .post("/api/groups", alice, json!({ "name": &"x".repeat(200) }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn non_member_gets_404_everywhere() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    let id = create(&fixture, alice, "Team").await;

    for (status, body) in [
        fixture
            .get(&format!("/api/groups/{id}/members"), Some(bob))
            .await,
        fixture
            .patch(&format!("/api/groups/{id}"), bob, json!({ "name": "Mine" }))
            .await,
        fixture.delete(&format!("/api/groups/{id}"), bob).await,
        fixture
            .delete(&format!("/api/groups/{id}/members/{alice}"), bob)
            .await,
    ] {
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["detail"], format!("Group {id} not found"));
    }
}

#[tokio::test]
async fn member_cannot_rename_or_delete_403() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    let id = create(&fixture, alice, "Team").await;
    join(&fixture.pool, &id, bob, "member").await;

    let (status, body) = fixture
        .patch(&format!("/api/groups/{id}"), bob, json!({ "name": "Mine" }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, body) = fixture.delete(&format!("/api/groups/{id}"), bob).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(members_of(&fixture.pool, &id).await, 2);
}

#[tokio::test]
async fn members_listed_by_email() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    let carol = fixture.user("carol@example.org").await;
    let id = create(&fixture, alice, "Team").await;
    join(&fixture.pool, &id, carol, "member").await;
    join(&fixture.pool, &id, bob, "member").await;

    let (status, body) = fixture
        .get(&format!("/api/groups/{id}/members"), Some(bob))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "members": [
            { "user_id": alice.to_string(), "email": "alice@example.org", "role": "owner" },
            { "user_id": bob.to_string(), "email": "bob@example.org", "role": "member" },
            { "user_id": carol.to_string(), "email": "carol@example.org", "role": "member" },
        ] })
    );
}

#[tokio::test]
async fn member_can_leave() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    let id = create(&fixture, alice, "Team").await;
    join(&fixture.pool, &id, bob, "member").await;

    let (status, _) = fixture
        .delete(&format!("/api/groups/{id}/members/{bob}"), bob)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(members_of(&fixture.pool, &id).await, 1);

    let (status, body) = fixture
        .get(&format!("/api/groups/{id}/members"), Some(bob))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn owner_cannot_leave_409() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let id = create(&fixture, alice, "Team").await;

    let (status, body) = fixture
        .delete(&format!("/api/groups/{id}/members/{alice}"), alice)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["detail"], "The owner cannot leave the group");
    assert_eq!(members_of(&fixture.pool, &id).await, 1);
}

#[tokio::test]
async fn owner_removes_member_and_their_sockets_close() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    fixture.room("room-shared", alice).await;
    let id = create(&fixture, alice, "Team").await;
    join(&fixture.pool, &id, bob, "member").await;
    grant(&fixture.pool, "room-shared", &id, "editor").await;

    let room = fixture.state.rooms.get_or_open("room-shared").unwrap();
    let (out, mut inbox) = outbox();
    room.attach(1, bob, Role::Editor, out).unwrap();

    let (status, _) = fixture
        .delete(&format!("/api/groups/{id}/members/{bob}"), alice)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let closed = timeout(WAIT, &mut inbox.close)
        .await
        .expect("the room closed the socket of the member who left");
    assert_eq!(closed.ok(), Some(CloseReason::Revoked));
    assert_eq!(members_of(&fixture.pool, &id).await, 1);
}

#[tokio::test]
async fn delete_group_revokes_every_member_in_every_room() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    let carol = fixture.user("carol@example.org").await;
    fixture.room("room-one", alice).await;
    fixture.room("room-two", alice).await;
    let id = create(&fixture, alice, "Team").await;
    join(&fixture.pool, &id, bob, "member").await;
    join(&fixture.pool, &id, carol, "member").await;
    grant(&fixture.pool, "room-one", &id, "editor").await;
    grant(&fixture.pool, "room-two", &id, "viewer").await;

    let mut inboxes = Vec::new();
    for (room_id, role) in [("room-one", Role::Editor), ("room-two", Role::Viewer)] {
        let room = fixture.state.rooms.get_or_open(room_id).unwrap();
        for (client, user) in [(1, alice), (2, bob), (3, carol)] {
            let (out, inbox) = outbox();
            room.attach(client, user, role, out).unwrap();
            inboxes.push(inbox);
        }
    }

    let (status, _) = fixture.delete(&format!("/api/groups/{id}"), alice).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    for inbox in &mut inboxes {
        let closed = timeout(WAIT, &mut inbox.close)
            .await
            .expect("the room closed the socket of a member of the deleted group");
        assert_eq!(closed.ok(), Some(CloseReason::Revoked));
    }
    let grants: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM room_group")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(grants, 0);
    let (status, _) = fixture
        .get(&format!("/api/groups/{id}/members"), Some(alice))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn anonymous_401() {
    let fixture = Fixture::start().await;
    let (status, body) = fixture.get("/api/groups", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}
