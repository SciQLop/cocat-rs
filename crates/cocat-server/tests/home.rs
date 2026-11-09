//! The home page's data: what waits for the caller and what others changed in their projects.

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use bytes::Bytes;
use cocat_core::model::{self, EventRecord};
use cocat_core::role::Role;
use cocat_core::store::FileRoomStore;
use cocat_core::time::Timestamp;
use cocat_server::history::stamps::{StampFile, stamp_for};
use cocat_server::room::RoomManager;
use cocat_server::submissions::{self, Decision, NewSubmission};
use cocat_server::{
    AppState, auth, db, invitations, links::Links, presence, ratelimit::LoginLimit, ws,
};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tower::ServiceExt;
use uuid::Uuid;
use yrs::{Doc, ReadTxn, StateVector, Transact};

const LAB: &str = "lab";
const MINUTE: i64 = 60_000;
const DAY: i64 = 24 * 60 * MINUTE;

fn now_ms() -> i64 {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    i64::try_from(since.as_millis()).unwrap()
}

/// Alice owns Lab, Bob edits it; Carol is a stranger to it.
struct World {
    state: AppState,
    update_dir: PathBuf,
    alice: Uuid,
    bob: Uuid,
    carol: Uuid,
    _dir: TempDir,
}

impl World {
    async fn start() -> Self {
        Self::with_budget(None).await
    }

    async fn with_budget(budget: Option<u64>) -> Self {
        let dir = TempDir::new().unwrap();
        let update_dir = dir.path().join("rooms");
        fs::create_dir_all(&update_dir).unwrap();
        let pool = db::open(&dir.path().join("test.db")).await.unwrap();
        let state = AppState {
            pool,
            secret: "test-secret".to_owned(),
            rooms: RoomManager::new(update_dir.clone(), budget),
            before_attach: None,
            login_limit: LoginLimit::default(),
            mailer: None,
            links: Links::default(),
            oidc: Vec::new(),
            web_dir: None,
        };
        let user =
            |email: &'static str| db::create_user(&state.pool, email, "hunter2", false, true);
        let alice = user("alice@x.org").await.unwrap();
        let bob = user("bob@x.org").await.unwrap();
        let carol = user("carol@x.org").await.unwrap();
        let world = Self {
            state,
            update_dir,
            alice,
            bob,
            carol,
            _dir: dir,
        };
        world.project(LAB, "Lab", alice).await;
        world.join(LAB, bob, Role::Editor).await;
        world
    }

    async fn project(&self, id: &str, name: &str, owner: Uuid) {
        db::create_room(&self.state.pool, owner, id, name)
            .await
            .unwrap();
    }

    async fn join(&self, room: &str, user: Uuid, role: Role) {
        db::add_member(&self.state.pool, room, user, role)
            .await
            .unwrap();
    }

    /// Appends one record adding event `uuid` to `room`, stamped with `user` at `at_ms`.
    fn write(&self, room: &str, user: Uuid, at_ms: i64, uuid: &str) {
        let (mut store, records) = FileRoomStore::open(&self.update_dir, room).unwrap();
        let doc = Doc::new();
        let at = Timestamp::parse("2020-01-01T00:00:00Z").unwrap();
        model::create_event(&mut doc.transact_mut(), &EventRecord::new(uuid, at, at)).unwrap();
        let record = Bytes::from(
            doc.transact()
                .encode_state_as_update_v1(&StateVector::default()),
        );
        let index = records.len() as u64;
        store.append(record.clone()).unwrap();
        store.flush().unwrap();
        let mut stamps = StampFile::open(&self.update_dir, room).unwrap();
        stamps.append(stamp_for(index, Some(user), at_ms, &record));
        stamps.write().unwrap();
    }

    async fn seen(&self, room: &str, user: Uuid, at_ms: i64) {
        db::touch_room_seen(&self.state.pool, user, room, at_ms)
            .await
            .unwrap();
    }

    async fn call(&self, method: Method, path: &str, user: Uuid) -> (StatusCode, Value) {
        let token = auth::start_session(&self.state.pool, user).await.unwrap();
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::COOKIE, format!("fastapiusersauth={token}"))
            .body(Body::empty())
            .unwrap();
        let response = ws::router(self.state.clone())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, body)
    }

    async fn home(&self, user: Uuid) -> Value {
        let (status, body) = self.call(Method::GET, "/api/me/home", user).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    async fn project_card(&self, user: Uuid, room: &str) -> Value {
        let home = self.home(user).await;
        home["projects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|project| project["id"] == room)
            .cloned()
            .unwrap_or_else(|| panic!("{room} is not on the home page: {home}"))
    }

    async fn invite(&self, email: &str, role: Role) -> i64 {
        invitations::create(&self.state, LAB, self.alice, role, Some(email), 14, None)
            .await
            .unwrap()
            .0
    }

    async fn sql(&self, statement: &'static str, id: i64) {
        sqlx::query(statement)
            .bind(id)
            .execute(&self.state.pool)
            .await
            .unwrap();
    }

    async fn submission(&self, author: Uuid, title: &str) -> i64 {
        let new = NewSubmission {
            author,
            room: LAB,
            kind: "new",
            target: None,
            base: None,
            catalogue: "c",
            title,
            folder: None,
            description: None,
            note: None,
            snapshot: b"",
        };
        submissions::insert(&self.state.pool, &new).await.unwrap()
    }

    async fn decide(&self, id: i64, status: &'static str, by: Uuid) {
        let decision = Decision {
            status,
            by,
            note: Some("thanks"),
        };
        assert!(
            submissions::decide(&self.state.pool, id, &decision)
                .await
                .unwrap()
        );
    }
}

fn titles(listed: &Value) -> Vec<String> {
    listed
        .as_array()
        .unwrap()
        .iter()
        .map(|submission| submission["title"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn opening_a_project_marks_it_seen() {
    let world = World::start().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = world.state.clone();
    tokio::spawn(async move { axum::serve(listener, ws::router(served)).await.unwrap() });
    let token = auth::start_session(&world.state.pool, world.bob)
        .await
        .unwrap();
    let mut request = format!("ws://{addr}/room/{LAB}")
        .into_client_request()
        .unwrap();
    let headers = request.headers_mut();
    headers.insert(
        "cookie",
        format!("fastapiusersauth={token}").parse().unwrap(),
    );
    headers.insert(
        header::SEC_WEBSOCKET_PROTOCOL,
        cocat_core::SUBPROTOCOL.parse().unwrap(),
    );
    let before = now_ms();

    let (_socket, response) = connect_async(request).await.unwrap();

    assert_eq!(response.status(), 101);
    let card = world.project_card(world.bob, LAB).await;
    let seen = card["seen_ms"]
        .as_i64()
        .expect("the project was not marked seen");
    assert!(
        (before..=now_ms()).contains(&seen),
        "{seen} not in [{before}, now]"
    );
    assert_eq!(
        world.project_card(world.alice, LAB).await["seen_ms"],
        Value::Null
    );
}

#[tokio::test]
async fn home_lists_what_others_changed_since_the_project_was_seen() {
    let world = World::start().await;
    let seen = now_ms() - DAY;
    world.write(LAB, world.bob, seen - 20 * MINUTE, "old");
    for n in 0..7 {
        world.write(
            LAB,
            world.bob,
            seen + (n + 1) * 10 * MINUTE,
            &format!("e{n}"),
        );
    }
    world.seen(LAB, world.alice, seen).await;

    let card = world.project_card(world.alice, LAB).await;

    let changes = card["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 5, "{card}");
    assert_eq!(card["more"], 2);
    assert_eq!(card["seen_ms"], seen);
    assert_eq!(card["name"], "Lab");
    assert_eq!(card["role"], "owner");
    assert_eq!(card["last_change_ms"], seen + 70 * MINUTE);
    assert_eq!(changes[0]["started_ms"], seen + 70 * MINUTE);
    assert_eq!(changes[4]["started_ms"], seen + 30 * MINUTE);
    assert_eq!(changes[0]["author_email"], "bob@x.org");
    assert_eq!(changes[0]["author_color"], presence::color("bob@x.org"));
    assert_eq!(changes[0]["events_added"], 1);
}

#[tokio::test]
async fn ones_own_changes_are_not_news() {
    let world = World::start().await;
    let seen = now_ms() - DAY;
    world.seen(LAB, world.alice, seen).await;
    world.write(LAB, world.alice, seen + MINUTE, "mine");

    let card = world.project_card(world.alice, LAB).await;

    assert_eq!(card["changes"], json!([]));
    assert_eq!(card["more"], 0);
    assert_eq!(card["last_change_ms"], seen + MINUTE);
}

#[tokio::test]
async fn a_project_never_opened_counts_the_last_thirty_days() {
    let world = World::start().await;
    let now = now_ms();
    world.write(LAB, world.bob, now - 40 * DAY, "ancient");
    world.write(LAB, world.bob, now - DAY, "recent");

    let card = world.project_card(world.alice, LAB).await;

    let changes = card["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1, "{card}");
    assert_eq!(changes[0]["started_ms"], now - DAY);
    assert_eq!(card["seen_ms"], Value::Null);
}

#[tokio::test]
async fn nothing_older_than_thirty_days_is_news_even_unseen_since() {
    let world = World::start().await;
    let now = now_ms();
    world.seen(LAB, world.alice, now - 60 * DAY).await;
    world.write(LAB, world.bob, now - 40 * DAY, "ancient");

    let card = world.project_card(world.alice, LAB).await;

    assert_eq!(card["changes"], json!([]));
}

#[tokio::test]
async fn projects_come_most_recently_changed_first() {
    let world = World::start().await;
    let now = now_ms();
    world.project("quiet", "Quiet", world.alice).await;
    world.project("busy", "Busy", world.alice).await;
    world.project("carols", "Carol's", world.carol).await;
    world.write("carols", world.carol, now, "c");
    world.write(LAB, world.bob, now - 2 * DAY, "a");
    world.write("busy", world.alice, now - DAY, "b");

    let home = world.home(world.alice).await;

    let order: Vec<&str> = home["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|project| project["id"].as_str().unwrap())
        .collect();
    assert_eq!(order, ["busy", LAB, "quiet"]);
}

#[tokio::test]
async fn home_shows_the_invitations_addressed_to_the_caller() {
    let world = World::start().await;
    let mine = world.invite("carol@x.org", Role::Viewer).await;
    world.invite("dave@x.org", Role::Viewer).await;
    let expired = world.invite("carol@x.org", Role::Editor).await;
    world
        .sql(
            "UPDATE invitation SET expires_at = '2000-01-01T00:00:00.000Z' WHERE id = ?",
            expired,
        )
        .await;
    let revoked = world.invite("carol@x.org", Role::Editor).await;
    invitations::revoke(&world.state, LAB, world.alice, revoked)
        .await
        .unwrap();

    let home = world.home(world.carol).await;

    let listed = home["invitations"].as_array().unwrap();
    assert_eq!(listed.len(), 1, "{home}");
    assert_eq!(listed[0]["id"], mine);
    assert_eq!(listed[0]["room_id"], LAB);
    assert_eq!(listed[0]["room_name"], "Lab");
    assert_eq!(listed[0]["role"], "viewer");
    assert_eq!(listed[0]["inviter_email"], "alice@x.org");
    assert_eq!(world.home(world.bob).await["invitations"], json!([]));
}

#[tokio::test]
async fn accepting_an_invitation_joins_with_its_role_and_uses_it() {
    let world = World::start().await;
    let id = world.invite("carol@x.org", Role::Viewer).await;

    let (status, body) = world
        .call(
            Method::POST,
            &format!("/api/invitations/{id}/accept"),
            world.carol,
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "id": LAB, "name": "Lab", "role": "viewer" }));
    let role = db::role_in(&world.state.pool, LAB, world.carol)
        .await
        .unwrap();
    assert_eq!(role, Some(Role::Viewer));
    assert_eq!(world.home(world.carol).await["invitations"], json!([]));
    let (again, _) = world
        .call(
            Method::POST,
            &format!("/api/invitations/{id}/accept"),
            world.carol,
        )
        .await;
    assert_eq!(again, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn declining_an_invitation_revokes_it() {
    let world = World::start().await;
    let id = world.invite("carol@x.org", Role::Viewer).await;

    let (status, body) = world
        .call(
            Method::POST,
            &format!("/api/invitations/{id}/decline"),
            world.carol,
        )
        .await;

    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(world.home(world.carol).await["invitations"], json!([]));
    let listed = invitations::list(&world.state, LAB, world.alice)
        .await
        .unwrap();
    assert!(listed.is_empty(), "{listed:?}");
    let (accept, _) = world
        .call(
            Method::POST,
            &format!("/api/invitations/{id}/accept"),
            world.carol,
        )
        .await;
    assert_eq!(accept, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn someone_elses_invitation_is_not_found() {
    let world = World::start().await;
    let id = world.invite("dave@x.org", Role::Viewer).await;

    for action in ["accept", "decline"] {
        let (status, _) = world
            .call(
                Method::POST,
                &format!("/api/invitations/{id}/{action}"),
                world.carol,
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{action}");
    }
    assert_eq!(
        db::role_in(&world.state.pool, LAB, world.carol)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        invitations::list(&world.state, LAB, world.alice)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn an_unverified_account_cannot_take_an_invitation_to_its_address() {
    let world = World::start().await;
    let dave = db::create_user(&world.state.pool, "dave@x.org", "hunter2", false, false)
        .await
        .unwrap();
    let id = world.invite("dave@x.org", Role::Viewer).await;

    assert_eq!(world.home(dave).await["invitations"], json!([]));
    let (status, _) = world
        .call(Method::POST, &format!("/api/invitations/{id}/accept"), dave)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn curators_see_the_submissions_waiting_for_review() {
    let world = World::start().await;
    db::set_curator(&world.state.pool, world.carol, true)
        .await
        .unwrap();
    world.submission(world.bob, "waiting").await;
    let decided = world.submission(world.bob, "decided").await;
    world
        .decide(decided, submissions::ACCEPTED, world.carol)
        .await;

    assert_eq!(
        titles(&world.home(world.carol).await["reviews"]),
        ["waiting"]
    );
    assert_eq!(world.home(world.alice).await["reviews"], json!([]));
}

#[tokio::test]
async fn authors_see_their_pending_and_recently_decided_submissions() {
    let world = World::start().await;
    world.submission(world.bob, "pending").await;
    let accepted = world.submission(world.bob, "accepted").await;
    world
        .decide(accepted, submissions::ACCEPTED, world.carol)
        .await;
    let declined = world.submission(world.bob, "declined").await;
    world
        .decide(declined, submissions::DECLINED, world.carol)
        .await;
    let old = world.submission(world.bob, "old").await;
    world.decide(old, submissions::ACCEPTED, world.carol).await;
    world
        .sql(
            "UPDATE submission SET decided_at = '2000-01-01T00:00:00Z' WHERE id = ?",
            old,
        )
        .await;
    let withdrawn = world.submission(world.bob, "withdrawn").await;
    world
        .decide(withdrawn, submissions::WITHDRAWN, world.bob)
        .await;
    world.submission(world.alice, "alices").await;

    let mine = &world.home(world.bob).await["submissions"];

    assert_eq!(titles(mine), ["pending", "declined", "accepted"]);
    assert_eq!(mine[1]["review_note"], "thanks");
    assert_eq!(mine[1]["reviewer"], "a curator");
}

#[tokio::test]
async fn a_project_too_big_to_read_still_has_its_card() {
    let world = World::with_budget(Some(1)).await;
    world.write(LAB, world.bob, now_ms() - MINUTE, "too big");

    let card = world.project_card(world.alice, LAB).await;

    assert_eq!(card["name"], "Lab");
    assert_eq!(card["changes"], Value::Array(Vec::new()));
}

#[tokio::test]
async fn leaving_a_project_marks_it_seen() {
    let world = World::start().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = world.state.clone();
    tokio::spawn(async move { axum::serve(listener, ws::router(served)).await.unwrap() });
    let token = auth::start_session(&world.state.pool, world.bob)
        .await
        .unwrap();
    let mut request = format!("ws://{addr}/room/{LAB}")
        .into_client_request()
        .unwrap();
    let headers = request.headers_mut();
    headers.insert(
        "cookie",
        format!("fastapiusersauth={token}").parse().unwrap(),
    );
    headers.insert(
        header::SEC_WEBSOCKET_PROTOCOL,
        cocat_core::SUBPROTOCOL.parse().unwrap(),
    );
    let (mut socket, _) = connect_async(request).await.unwrap();
    let opened = world.project_card(world.bob, LAB).await["seen_ms"]
        .as_i64()
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    socket.close(None).await.unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let seen = world.project_card(world.bob, LAB).await["seen_ms"]
            .as_i64()
            .unwrap();
        if seen > opened {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "leaving did not mark it seen"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}
