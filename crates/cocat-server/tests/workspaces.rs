use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use cocat_core::role::Role;
use cocat_server::room::RoomManager;
use cocat_server::{AppState, auth, db, links::Links, ratelimit::LoginLimit, ws};
use serde_json::{Value, json};
use std::fs;
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
            login_limit: LoginLimit::default(),
            mailer: None,
            links: Links::default(),
            oidc: Vec::new(),
            web_dir: None,
        };
        Self { state, _dir: dir }
    }

    async fn user_token(&self, email: &str) -> (Uuid, String) {
        let user = db::create_user(&self.state.pool, email, "hunter2", false, true)
            .await
            .unwrap();
        let token = auth::start_session(&self.state.pool, user).await.unwrap();
        (user, token)
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        token: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::COOKIE, format!("fastapiusersauth={token}"));
        let body = match body {
            Some(body) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        let router: Router = ws::router(self.state.clone());
        let response = router.oneshot(request.body(body).unwrap()).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body)
    }

    async fn project(&self, token: &str, kind: &str) -> String {
        let (status, body) = self
            .send(
                Method::POST,
                "/api/rooms",
                token,
                Some(json!({"name": "Lab", "kind": kind})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["id"].as_str().unwrap().to_owned()
    }

    async fn add_document(&self, token: &str, room: &str, kind: &str) -> (StatusCode, Value) {
        self.send(
            Method::POST,
            &format!("/api/rooms/{room}/documents"),
            token,
            Some(json!({"kind": kind})),
        )
        .await
    }
}

#[tokio::test]
async fn a_workspace_starts_with_its_manifest() {
    let fixture = Fixture::start().await;
    let (_, token) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&token, "workspace").await;

    let (status, body) = fixture
        .send(
            Method::GET,
            &format!("/api/rooms/{room}/documents"),
            &token,
            None,
        )
        .await;

    assert_eq!(status, StatusCode::OK);
    let documents = body.as_array().unwrap();
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0]["kind"], "manifest");
    assert!(documents[0]["id"].is_string());
    assert!(documents[0]["created_by"].is_string());
    assert!(documents[0]["created_at"].is_string());
}

#[tokio::test]
async fn an_editor_adds_a_notebook() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let (editor, editor_token) = fixture.user_token("editor@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    db::add_member(&fixture.state.pool, &room, editor, Role::Editor)
        .await
        .unwrap();

    let (status, body) = fixture.add_document(&editor_token, &room, "notebook").await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["kind"], "notebook");
    let id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        db::document_kind(&fixture.state.pool, &room, id)
            .await
            .unwrap(),
        Some(db::DocumentKind::Notebook)
    );
    let (_, listed) = fixture
        .send(
            Method::GET,
            &format!("/api/rooms/{room}/documents"),
            &owner,
            None,
        )
        .await;
    assert_eq!(listed.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn a_viewer_cannot_add_a_document() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let (viewer, viewer_token) = fixture.user_token("viewer@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    db::add_member(&fixture.state.pool, &room, viewer, Role::Viewer)
        .await
        .unwrap();

    let (status, _) = fixture.add_document(&viewer_token, &room, "text").await;
    let (read, _) = fixture
        .send(
            Method::GET,
            &format!("/api/rooms/{room}/documents"),
            &viewer_token,
            None,
        )
        .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(read, StatusCode::OK);
}

#[tokio::test]
async fn a_catalogue_project_holds_no_documents() {
    let fixture = Fixture::start().await;
    let (_, token) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&token, "catalogues").await;

    let (status, body) = fixture.add_document(&token, &room, "notebook").await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["detail"].is_string());
}

#[tokio::test]
async fn nobody_adds_a_second_manifest() {
    let fixture = Fixture::start().await;
    let (_, token) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&token, "workspace").await;

    let (status, _) = fixture.add_document(&token, &room, "manifest").await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_project_list_says_each_kind() {
    let fixture = Fixture::start().await;
    let (_, token) = fixture.user_token("owner@example.org").await;
    let workspace = fixture.project(&token, "workspace").await;
    let (status, created) = fixture
        .send(
            Method::POST,
            "/api/rooms",
            &token,
            Some(json!({"name": "Plain"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["kind"], "catalogues");

    let (_, listed) = fixture.send(Method::GET, "/api/rooms", &token, None).await;

    let kind_of = |id: &str| {
        listed
            .as_array()
            .unwrap()
            .iter()
            .find(|room| room["id"] == id)
            .map(|room| room["kind"].clone())
    };
    assert_eq!(kind_of(&workspace), Some(json!("workspace")));
    assert_eq!(
        kind_of(created["id"].as_str().unwrap()),
        Some(json!("catalogues"))
    );
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .all(|room| room["kind"].is_string())
    );
}
