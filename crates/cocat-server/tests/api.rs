use std::fs;
use std::time::SystemTime;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderValue, Request, StatusCode, header};
use cocat_core::role::Role;
use cocat_server::room::RoomManager;
use cocat_server::{AppState, auth, db, ws};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    state: AppState,
    _dir: TempDir,
}

impl Fixture {
    async fn start() -> Self {
        let dir = TempDir::new().unwrap();
        let rooms = dir.path().join("rooms");
        fs::create_dir_all(&rooms).unwrap();
        let pool = db::open(&dir.path().join("test.db")).await.unwrap();
        let state = AppState {
            pool,
            secret: "test-secret".to_owned(),
            rooms: RoomManager::new(rooms, None),
            before_attach: None,
        };
        Self { state, _dir: dir }
    }

    async fn user(&self, email: &str) -> Uuid {
        db::create_user(&self.state.pool, email, "hunter2", false)
            .await
            .unwrap()
    }

    fn token(&self, user: Uuid) -> String {
        auth::mint_token(user, &self.state.secret, SystemTime::now()).unwrap()
    }

    fn router(&self) -> Router {
        ws::router(self.state.clone())
    }

    async fn get(&self, path: &str, token: Option<&str>) -> (StatusCode, Value) {
        let mut request = Request::builder().uri(path);
        if let Some(token) = token {
            request = request.header(
                header::COOKIE,
                HeaderValue::from_str(&format!("fastapiusersauth={token}")).unwrap(),
            );
        }
        let response = self
            .router()
            .oneshot(request.body(Body::empty()).unwrap())
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
}

#[tokio::test]
async fn me_without_a_cookie_is_401() {
    let fixture = Fixture::start().await;

    let (status, body) = fixture.get("/api/me", None).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, json!({"detail": "Unauthorized"}));
}

#[tokio::test]
async fn me_returns_the_user() {
    let fixture = Fixture::start().await;
    let user = fixture.user("owner@example.org").await;

    let (status, body) = fixture.get("/api/me", Some(&fixture.token(user))).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], user.to_string());
    assert_eq!(body["email"], "owner@example.org");
    assert_eq!(body["is_active"], true);
    assert_eq!(body["is_superuser"], false);
    assert_eq!(body["is_verified"], true);
}

#[tokio::test]
async fn legacy_rooms_lists_room_ids() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    db::create_room(&fixture.state.pool, owner, "zeta", "Zeta")
        .await
        .unwrap();
    db::create_room(&fixture.state.pool, owner, "alpha", "Alpha")
        .await
        .unwrap();

    let (status, body) = fixture.get("/rooms", Some(&fixture.token(owner))).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"rooms": ["alpha", "zeta"]}));
}

#[tokio::test]
async fn legacy_room_users_needs_membership() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let member = fixture.user("member@example.org").await;
    let outsider = fixture.user("outsider@example.org").await;
    db::create_room(&fixture.state.pool, owner, "room-1", "Room")
        .await
        .unwrap();
    db::add_member(&fixture.state.pool, "room-1", member, Role::Viewer)
        .await
        .unwrap();

    let (denied, _) = fixture
        .get("/room/room-1/users", Some(&fixture.token(outsider)))
        .await;
    let (status, body) = fixture
        .get("/room/room-1/users", Some(&fixture.token(member)))
        .await;

    assert_eq!(denied, StatusCode::NOT_FOUND);
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"users": ["member@example.org", "owner@example.org"]})
    );
}
