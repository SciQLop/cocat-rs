use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use cocat_core::role::Role;
use cocat_server::mail::Mailer;
use cocat_server::room::{CloseReason, RoomManager, outbox};
use cocat_server::{AppState, auth, db, links::Links, ratelimit::LoginLimit, ws};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::time::timeout;
use tower::ServiceExt;
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(10);
const PUBLIC_URL: &str = "https://cocat.example.org";

struct Fixture {
    state: AppState,
    pool: SqlitePool,
    mail_dir: Option<PathBuf>,
    _dir: TempDir,
}

impl Fixture {
    async fn start() -> Self {
        Self::build(false).await
    }

    async fn start_with_mail() -> Self {
        Self::build(true).await
    }

    async fn build(mail: bool) -> Self {
        let dir = TempDir::new().unwrap();
        let rooms = dir.path().join("rooms");
        fs::create_dir_all(&rooms).unwrap();
        let mail_dir = mail.then(|| dir.path().join("mail"));
        let mut env: HashMap<&str, String> = HashMap::new();
        if let Some(path) = &mail_dir {
            fs::create_dir_all(path).unwrap();
            env.insert("COCAT_MAIL_DIR", path.to_string_lossy().into_owned());
            env.insert("COCAT_MAIL_FROM", "cocat@example.org".to_owned());
            env.insert("COCAT_PUBLIC_URL", PUBLIC_URL.to_owned());
        }
        let lookup = |name: &str| env.get(name).cloned();
        let pool = db::open(&dir.path().join("test.db")).await.unwrap();
        let state = AppState {
            pool: pool.clone(),
            secret: "test-secret".to_owned(),
            rooms: RoomManager::new(rooms, None),
            before_attach: None,
            login_limit: LoginLimit::default(),
            mailer: Mailer::from_env(&lookup).unwrap(),
            links: Links::from_env(&lookup).unwrap(),
            oidc: Vec::new(),
        };
        Self {
            state,
            pool,
            mail_dir,
            _dir: dir,
        }
    }

    fn mails(&self) -> Vec<String> {
        let Some(dir) = &self.mail_dir else {
            return Vec::new();
        };
        let mut mails: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect();
        mails.sort();
        mails
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

    async fn put(&self, path: &str, user: Uuid, body: Value) -> (StatusCode, Value) {
        self.send(Method::PUT, path, Some(user), Some(body)).await
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
async fn group_name_with_newline_refused() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;

    for name in ["a\nb", "a\u{1}b"] {
        let (status, body) = fixture
            .post("/api/groups", alice, json!({ "name": name }))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name:?}");
        assert_eq!(
            body["detail"], "Enter a group name without control characters",
            "{body}"
        );
    }

    let id = create(&fixture, alice, "Team").await;
    let (status, body) = fixture
        .patch(
            &format!("/api/groups/{id}"),
            alice,
            json!({ "name": "a\nb" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["detail"],
        "Enter a group name without control characters"
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

#[tokio::test]
async fn a_member_joined_just_before_the_delete_is_revoked() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    fixture.room("room-late", alice).await;
    let id = create(&fixture, alice, "Team").await;

    let room = fixture.state.rooms.get_or_open("room-late").unwrap();
    let (out, mut inbox) = outbox();
    room.attach(1, bob, Role::Editor, out).unwrap();

    join(&fixture.pool, &id, bob, "member").await;
    grant(&fixture.pool, "room-late", &id, "editor").await;

    let (status, _) = fixture.delete(&format!("/api/groups/{id}"), alice).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let closed = timeout(WAIT, &mut inbox.close)
        .await
        .expect("the delete must revoke a member it found in the same transaction");
    assert_eq!(closed.ok(), Some(CloseReason::Revoked));
}

#[tokio::test]
async fn owner_grants_and_members_get_access() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    fixture.room("room-granted", alice).await;
    let id = create(&fixture, alice, "Team").await;
    join(&fixture.pool, &id, bob, "member").await;

    let (status, body) = fixture
        .put(
            &format!("/api/rooms/room-granted/groups/{id}"),
            alice,
            json!({ "role": "editor" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "id": id, "name": "Team", "role": "editor" }));

    let (status, body) = fixture.get("/api/rooms", Some(bob)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!([{ "id": "room-granted", "name": "room-granted", "role": "editor" }])
    );

    let (status, body) = fixture.get("/room/room-granted/users", Some(bob)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn grant_to_a_foreign_group_is_404() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    fixture.room("room-mine", alice).await;
    let id = create(&fixture, bob, "Bobs team").await;

    let (status, body) = fixture
        .put(
            &format!("/api/rooms/room-mine/groups/{id}"),
            alice,
            json!({ "role": "viewer" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["detail"], format!("Group {id} not found"));
}

#[tokio::test]
async fn editor_through_group_cannot_do_owner_things() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    fixture.room("room-guarded", alice).await;
    let id = create(&fixture, alice, "Team").await;
    join(&fixture.pool, &id, bob, "member").await;
    grant(&fixture.pool, "room-guarded", &id, "editor").await;

    for (status, body) in [
        fixture.delete("/api/rooms/room-guarded", bob).await,
        fixture
            .post(
                "/api/rooms/room-guarded/invitations",
                bob,
                json!({ "role": "viewer" }),
            )
            .await,
        fixture
            .patch(
                &format!("/api/rooms/room-guarded/members/{alice}"),
                bob,
                json!({ "role": "viewer" }),
            )
            .await,
        fixture
            .put(
                &format!("/api/rooms/room-guarded/groups/{id}"),
                bob,
                json!({ "role": "viewer" }),
            )
            .await,
    ] {
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }
    assert_eq!(
        fixture
            .get("/api/rooms", Some(bob))
            .await
            .1
            .as_array()
            .unwrap()
            .len(),
        1,
        "the room is still there"
    );
}

#[tokio::test]
async fn owner_role_cannot_be_granted_422() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    fixture.room("room-owner-role", alice).await;
    let id = create(&fixture, alice, "Team").await;

    let (status, _) = fixture
        .put(
            &format!("/api/rooms/room-owner-role/groups/{id}"),
            alice,
            json!({ "role": "owner" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn changing_a_grant_revokes_the_group_in_that_room() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    fixture.room("room-changed", alice).await;
    let id = create(&fixture, alice, "Team").await;
    join(&fixture.pool, &id, bob, "member").await;
    grant(&fixture.pool, "room-changed", &id, "editor").await;

    let room = fixture.state.rooms.get_or_open("room-changed").unwrap();
    let (out, mut inbox) = outbox();
    room.attach(1, bob, Role::Editor, out).unwrap();

    let (status, _) = fixture
        .put(
            &format!("/api/rooms/room-changed/groups/{id}"),
            alice,
            json!({ "role": "viewer" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let closed = timeout(WAIT, &mut inbox.close)
        .await
        .expect("the room closed a socket that carried the old grant");
    assert_eq!(closed.ok(), Some(CloseReason::Revoked));
    let (status, body) = fixture.get("/api/rooms", Some(bob)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["role"], "viewer");
}

#[tokio::test]
async fn ungrant_revokes() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    fixture.room("room-ungranted", alice).await;
    let id = create(&fixture, alice, "Team").await;
    join(&fixture.pool, &id, bob, "member").await;
    grant(&fixture.pool, "room-ungranted", &id, "editor").await;

    let room = fixture.state.rooms.get_or_open("room-ungranted").unwrap();
    let (out, mut inbox) = outbox();
    room.attach(1, bob, Role::Editor, out).unwrap();

    let (status, _) = fixture
        .delete(&format!("/api/rooms/room-ungranted/groups/{id}"), alice)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let closed = timeout(WAIT, &mut inbox.close)
        .await
        .expect("the room closed a socket whose grant was removed");
    assert_eq!(closed.ok(), Some(CloseReason::Revoked));
    let (_, body) = fixture.get("/api/rooms", Some(bob)).await;
    assert_eq!(body, json!([]));
}

#[tokio::test]
async fn ungrant_unknown_is_404() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    fixture.room("room-none", alice).await;
    let id = create(&fixture, alice, "Team").await;

    let (status, body) = fixture
        .delete(&format!("/api/rooms/room-none/groups/{id}"), alice)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn room_groups_visible_to_viewers() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    let carol = fixture.user("carol@example.org").await;
    fixture.room("room-listed", alice).await;
    db::add_member(&fixture.pool, "room-listed", bob, Role::Viewer)
        .await
        .unwrap();
    let id = create(&fixture, alice, "Team").await;
    grant(&fixture.pool, "room-listed", &id, "editor").await;

    let (status, body) = fixture
        .get("/api/rooms/room-listed/groups", Some(bob))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "groups": [{ "id": id, "name": "Team", "role": "editor" }] })
    );

    let (status, _) = fixture
        .get("/api/rooms/room-listed/groups", Some(carol))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

async fn invite(
    fixture: &Fixture,
    group_id: &str,
    actor: Uuid,
    body: Value,
) -> (StatusCode, Value) {
    fixture
        .post(&format!("/api/groups/{group_id}/invitations"), actor, body)
        .await
}

async fn uses_of(pool: &SqlitePool, invitation_id: i64) -> i64 {
    sqlx::query_scalar("SELECT uses FROM group_invitation WHERE id = ?")
        .bind(invitation_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// lettre folds a long link across lines with quoted-printable soft breaks; they are undone
/// before a link is searched for.
fn flattened(mail: &str) -> String {
    mail.replace("=\r\n", "")
        .replace("=\n", "")
        .split_whitespace()
        .collect()
}

fn link_token(mails: &[String], marker: &str) -> String {
    for mail in mails {
        let body = mail.replace("=\r\n", "").replace("=\n", "");
        if let Some(start) = body.find(marker) {
            let token: String = body[start + marker.len()..]
                .chars()
                .take_while(|character| {
                    !character.is_whitespace() && !matches!(character, '"' | '<' | '>')
                })
                .collect();
            if !token.is_empty() {
                return token;
            }
        }
    }
    panic!("no {marker} link in {mails:?}");
}

#[tokio::test]
async fn accept_joins_as_member() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    fixture.room("room-join", alice).await;
    let id = create(&fixture, alice, "Team").await;
    grant(&fixture.pool, "room-join", &id, "viewer").await;

    let room = fixture.state.rooms.get_or_open("room-join").unwrap();
    let (out, mut inbox) = outbox();
    room.attach(1, bob, Role::Viewer, out).unwrap();

    let (status, body) = invite(&fixture, &id, alice, json!({})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let token = body["token"].as_str().unwrap().to_owned();

    let (status, body) = fixture
        .post(
            "/api/group-invitations/accept",
            bob,
            json!({ "token": token }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "id": id, "name": "Team", "role": "member" }));

    let (_, members) = fixture
        .get(&format!("/api/groups/{id}/members"), Some(alice))
        .await;
    assert_eq!(members["members"][1]["email"], "bob@example.org");
    assert_eq!(members["members"][1]["role"], "member");

    let closed = timeout(WAIT, &mut inbox.close)
        .await
        .expect("the rooms the group carries revoke the new member");
    assert_eq!(closed.ok(), Some(CloseReason::Revoked));
}

#[tokio::test]
async fn addressed_invitation_other_email_403() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    let id = create(&fixture, alice, "Team").await;

    let (status, body) = invite(
        &fixture,
        &id,
        alice,
        json!({ "email": "alice@example.org" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let token = body["token"].as_str().unwrap().to_owned();

    let (status, body) = fixture
        .post(
            "/api/group-invitations/accept",
            bob,
            json!({ "token": token }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        body["detail"],
        "This invitation is for another email address"
    );
}

#[tokio::test]
async fn expired_is_404() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    let id = create(&fixture, alice, "Team").await;

    let (status, body) = invite(&fixture, &id, alice, json!({})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let token = body["token"].as_str().unwrap().to_owned();
    let invitation_id = body["id"].as_i64().unwrap();
    sqlx::query("UPDATE group_invitation SET expires_at = '2000-01-01T00:00:00.000Z' WHERE id = ?")
        .bind(invitation_id)
        .execute(&fixture.pool)
        .await
        .unwrap();

    for (status, _) in [
        fixture
            .post(
                "/api/group-invitations/preview",
                bob,
                json!({ "token": token }),
            )
            .await,
        fixture
            .post(
                "/api/group-invitations/accept",
                bob,
                json!({ "token": token }),
            )
            .await,
    ] {
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn revoked_is_404() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    let id = create(&fixture, alice, "Team").await;

    let (status, body) = invite(&fixture, &id, alice, json!({})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let token = body["token"].as_str().unwrap().to_owned();
    let invitation_id = body["id"].as_i64().unwrap();

    let (status, _) = fixture
        .delete(
            &format!("/api/groups/{id}/invitations/{invitation_id}"),
            alice,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = fixture
        .post(
            "/api/group-invitations/accept",
            bob,
            json!({ "token": token }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn max_uses_spent_is_404() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    let carol = fixture.user("carol@example.org").await;
    let id = create(&fixture, alice, "Team").await;

    let (status, body) = invite(&fixture, &id, alice, json!({ "max_uses": 1 })).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let token = body["token"].as_str().unwrap().to_owned();

    let (status, _) = fixture
        .post(
            "/api/group-invitations/accept",
            bob,
            json!({ "token": token }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = fixture
        .post(
            "/api/group-invitations/accept",
            carol,
            json!({ "token": token }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn concurrent_accepts_respect_max_uses() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    let carol = fixture.user("carol@example.org").await;
    let id = create(&fixture, alice, "Team").await;

    let (status, body) = invite(&fixture, &id, alice, json!({ "max_uses": 1 })).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let token = body["token"].as_str().unwrap().to_owned();

    let (first, second) = tokio::join!(
        fixture.post(
            "/api/group-invitations/accept",
            bob,
            json!({ "token": token }),
        ),
        fixture.post(
            "/api/group-invitations/accept",
            carol,
            json!({ "token": token }),
        )
    );
    let accepted = [first.0, second.0]
        .iter()
        .filter(|status| **status == StatusCode::OK)
        .count();
    assert_eq!(accepted, 1, "one use admits one member");
    assert_eq!(
        members_of(&fixture.pool, &id).await,
        2,
        "the owner and one guest"
    );
}

#[tokio::test]
async fn already_member_spends_no_use() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    let carol = fixture.user("carol@example.org").await;
    let id = create(&fixture, alice, "Team").await;
    join(&fixture.pool, &id, bob, "member").await;

    let (status, body) = invite(&fixture, &id, alice, json!({ "max_uses": 1 })).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let token = body["token"].as_str().unwrap().to_owned();
    let invitation_id = body["id"].as_i64().unwrap();

    let (status, _) = fixture
        .post(
            "/api/group-invitations/accept",
            bob,
            json!({ "token": token }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(uses_of(&fixture.pool, invitation_id).await, 0);

    let (status, _) = fixture
        .post(
            "/api/group-invitations/accept",
            carol,
            json!({ "token": token }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(uses_of(&fixture.pool, invitation_id).await, 1);
}

#[tokio::test]
async fn mailed_invitation_hides_the_token() {
    let fixture = Fixture::start_with_mail().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    let id = create(&fixture, alice, "Team").await;

    let (status, body) = invite(&fixture, &id, alice, json!({ "email": "bob@example.org" })).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["token"], "");

    let mails = fixture.mails();
    assert_eq!(mails.len(), 1, "{mails:?}");
    assert!(mails[0].contains("To: bob@example.org"), "{mails:?}");
    assert!(
        mails[0].contains("alice@example.org invites you to the group Team"),
        "{mails:?}"
    );

    let token = link_token(&mails, "/ui/groups/join#");
    let (status, body) = fixture
        .post(
            "/api/group-invitations/accept",
            bob,
            json!({ "token": token }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["role"], "member");
}

#[tokio::test]
async fn link_uses_public_url() {
    let fixture = Fixture::start_with_mail().await;
    let alice = fixture.user("alice@example.org").await;
    let id = create(&fixture, alice, "Team").await;

    let (status, body) = invite(&fixture, &id, alice, json!({ "email": "bob@example.org" })).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let mail = flattened(&fixture.mails()[0]);
    assert!(
        mail.contains(&format!("{PUBLIC_URL}/ui/groups/join#")),
        "{mail}"
    );
    assert!(
        !mail.contains(&format!("{PUBLIC_URL}/ui/groups/join/")),
        "the token must not travel in the path: {mail}"
    );
}

#[tokio::test]
async fn non_owner_cannot_invite_403() {
    let fixture = Fixture::start().await;
    let alice = fixture.user("alice@example.org").await;
    let bob = fixture.user("bob@example.org").await;
    let carol = fixture.user("carol@example.org").await;
    let id = create(&fixture, alice, "Team").await;
    join(&fixture.pool, &id, bob, "member").await;

    let (status, body) = invite(&fixture, &id, alice, json!({})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let invitation_id = body["id"].as_i64().unwrap();

    for (status, body) in [
        invite(&fixture, &id, bob, json!({})).await,
        fixture
            .get(&format!("/api/groups/{id}/invitations"), Some(bob))
            .await,
        fixture
            .delete(
                &format!("/api/groups/{id}/invitations/{invitation_id}"),
                bob,
            )
            .await,
    ] {
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }

    for (status, _) in [
        invite(&fixture, &id, carol, json!({})).await,
        fixture
            .get(&format!("/api/groups/{id}/invitations"), Some(carol))
            .await,
    ] {
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
