use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use cocat_core::role::Role;
use cocat_server::room::{RoomManager, StorageLimits};
use cocat_server::{AppState, auth, blobs, db, links::Links, ratelimit::LoginLimit, ws};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

const MIB: u64 = 1024 * 1024;

struct Fixture {
    state: AppState,
    dir: PathBuf,
    _tmp: TempDir,
}

struct Reply {
    status: StatusCode,
    content_type: Option<String>,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

fn sha(body: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(&Sha256::digest(body))
}

impl Fixture {
    async fn start() -> Self {
        Self::with_limits(StorageLimits::default()).await
    }

    async fn with_limits(limits: StorageLimits) -> Self {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("rooms");
        fs::create_dir_all(&dir).unwrap();
        let pool = db::open(&tmp.path().join("test.db")).await.unwrap();
        let state = AppState {
            pool,
            secret: "test-secret".to_owned(),
            rooms: RoomManager::new(dir.clone(), None).with_storage(limits),
            before_attach: None,
            login_limit: LoginLimit::default(),
            mailer: None,
            links: Links::default(),
            oidc: Vec::new(),
            web_dir: None,
            providers: Default::default(),
            http: Default::default(),
        };
        Self {
            state,
            dir,
            _tmp: tmp,
        }
    }

    async fn user(&self, email: &str) -> (Uuid, String) {
        let user = db::create_user(&self.state.pool, email, "hunter2", false, true)
            .await
            .unwrap();
        let token = auth::start_session(&self.state.pool, user).await.unwrap();
        (user, token)
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        token: &str,
        body: Option<Value>,
        raw: Vec<u8>,
    ) -> Reply {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::COOKIE, format!("fastapiusersauth={token}"));
        let body = match body {
            Some(body) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(body.to_string())
            }
            None => Body::from(raw),
        };
        let router: Router = ws::router(self.state.clone());
        let response = router.oneshot(request.body(body).unwrap()).await.unwrap();
        Reply {
            status: response.status(),
            content_type: response
                .headers()
                .get(header::CONTENT_TYPE)
                .map(|value| value.to_str().unwrap().to_owned()),
            headers: response.headers().clone(),
            body: to_bytes(response.into_body(), 64 * 1024 * 1024)
                .await
                .unwrap()
                .to_vec(),
        }
    }

    async fn project(&self, token: &str, kind: &str) -> String {
        let reply = self
            .request(
                Method::POST,
                "/api/rooms",
                token,
                Some(json!({"name": "Lab", "kind": kind})),
                Vec::new(),
            )
            .await;
        assert_eq!(reply.status, StatusCode::CREATED);
        reply.json()["id"].as_str().unwrap().to_owned()
    }

    async fn put(&self, token: &str, room: &str, hash: &str, body: &[u8]) -> Reply {
        let path = format!("/api/rooms/{room}/blobs/{hash}");
        self.request(Method::PUT, &path, token, None, body.to_vec())
            .await
    }

    async fn get(&self, token: &str, room: &str, hash: &str) -> Reply {
        let path = format!("/api/rooms/{room}/blobs/{hash}");
        self.request(Method::GET, &path, token, None, Vec::new())
            .await
    }

    async fn member(&self, room: &str, email: &str, role: Role) -> String {
        let (user, token) = self.user(email).await;
        db::add_member(&self.state.pool, room, user, role)
            .await
            .unwrap();
        token
    }

    async fn stored(&self, room: &str) -> u64 {
        blobs::stored_bytes(&self.state.pool, room).await.unwrap()
    }

    fn files(&self) -> Vec<PathBuf> {
        let root = self.dir.join("blobs");
        let Ok(rooms) = fs::read_dir(root) else {
            return Vec::new();
        };
        rooms
            .flat_map(|room| fs::read_dir(room.unwrap().path()).unwrap())
            .map(|file| file.unwrap().path())
            .collect()
    }
}

async fn rows(fixture: &Fixture) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM blob")
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn an_editor_stores_and_a_viewer_reads_a_file() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user("owner@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    let editor = fixture
        .member(&room, "editor@example.org", Role::Editor)
        .await;
    let viewer = fixture
        .member(&room, "viewer@example.org", Role::Viewer)
        .await;
    let content = b"time,flux\n1,2\n";
    let hash = sha(content);

    let stored = fixture.put(&editor, &room, &hash, content).await;
    let read = fixture.get(&viewer, &room, &hash).await;

    assert_eq!(stored.status, StatusCode::CREATED);
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(read.body, content);
    assert_eq!(
        read.content_type.as_deref(),
        Some("application/octet-stream")
    );
    assert_eq!(fixture.stored(&room).await, content.len() as u64);
    assert_eq!(fixture.files().len(), 1);
    assert_eq!(
        fixture.get(&viewer, &room, &sha(b"other")).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_file_downloads_as_an_attachment_the_browser_does_not_sniff() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user("owner@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    let content = b"<html><script>alert(1)</script></html>";
    let hash = sha(content);
    fixture.put(&owner, &room, &hash, content).await;

    let read = fixture.get(&owner, &room, &hash).await;

    let header = |name: &str| read.headers.get(name).map(|value| value.to_str().unwrap());
    assert_eq!(header("x-content-type-options"), Some("nosniff"));
    assert_eq!(header("content-disposition"), Some("attachment"));
    assert_eq!(header("cache-control"), Some("private"));
}

#[tokio::test]
async fn a_damaged_or_lost_file_is_never_served_and_uploading_it_again_repairs_it() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user("owner@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    let content = b"time,flux\n1,2\n";
    let hash = sha(content);
    fixture.put(&owner, &room, &hash, content).await;
    let file = fixture.files().pop().unwrap();

    for damage in [Some(&b"garbage"[..]), Some(&b""[..]), None] {
        match damage {
            Some(bytes) => fs::write(&file, bytes).unwrap(),
            None => fs::remove_file(&file).unwrap(),
        }
        let read = fixture.get(&owner, &room, &hash).await;
        assert_eq!(read.status, StatusCode::INTERNAL_SERVER_ERROR, "{damage:?}");

        let again = fixture.put(&owner, &room, &hash, content).await;
        assert_eq!(again.status, StatusCode::OK, "{damage:?}");
        assert_eq!(fixture.get(&owner, &room, &hash).await.body, content);
    }
    assert_eq!(rows(&fixture).await, 1, "a repair records nothing new");
}

#[tokio::test]
async fn a_body_that_does_not_match_its_hash_is_refused() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user("owner@example.org").await;
    let room = fixture.project(&owner, "workspace").await;

    let mismatch = fixture.put(&owner, &room, &sha(b"one"), b"two").await;
    let malformed = fixture.put(&owner, &room, "ABC", b"two").await;

    assert_eq!(mismatch.status, StatusCode::BAD_REQUEST);
    assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
    assert!(fixture.files().is_empty());
    assert_eq!(rows(&fixture).await, 0);
}

#[tokio::test]
async fn the_same_file_twice_is_stored_once() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user("owner@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    let hash = sha(b"same");

    let first = fixture.put(&owner, &room, &hash, b"same").await;
    let before = fixture.stored(&room).await;
    let second = fixture.put(&owner, &room, &hash, b"same").await;

    assert_eq!(first.status, StatusCode::CREATED);
    assert_eq!(second.status, StatusCode::OK);
    assert_eq!(fixture.stored(&room).await, before);
    assert_eq!(rows(&fixture).await, 1);
    assert_eq!(fixture.files().len(), 1);
}

#[tokio::test]
async fn a_file_over_the_cap_is_refused() {
    let fixture = Fixture::with_limits(StorageLimits {
        file: MIB,
        ..StorageLimits::default()
    })
    .await;
    let (_, owner) = fixture.user("owner@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    let big = vec![7u8; MIB as usize + 1];
    let fits = vec![7u8; MIB as usize];

    let refused = fixture.put(&owner, &room, &sha(&big), &big).await;
    let accepted = fixture.put(&owner, &room, &sha(&fits), &fits).await;

    assert_eq!(refused.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(
        refused.json()["detail"]
            .as_str()
            .unwrap()
            .contains("at most")
    );
    assert_eq!(accepted.status, StatusCode::CREATED);
    assert_eq!(rows(&fixture).await, 1);
}

#[tokio::test]
async fn a_file_past_the_owner_quota_is_refused() {
    let fixture = Fixture::start().await;
    let (owner_id, owner) = fixture.user("owner@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    db::set_user_quota(&fixture.state.pool, owner_id, Some(1))
        .await
        .unwrap();
    let first = vec![1u8; (MIB / 2) as usize + 1];
    let second = vec![2u8; (MIB / 2) as usize + 1];

    let kept = fixture.put(&owner, &room, &sha(&first), &first).await;
    let refused = fixture.put(&owner, &room, &sha(&second), &second).await;

    assert_eq!(kept.status, StatusCode::CREATED);
    assert_eq!(refused.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(refused.json()["detail"].as_str().unwrap().contains("left"));
    assert_eq!(fixture.files().len(), 1);
    assert_eq!(rows(&fixture).await, 1);
}

#[tokio::test]
async fn files_count_toward_the_owner_quota_the_rooms_see() {
    let fixture = Fixture::start().await;
    let (owner_id, owner) = fixture.user("owner@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    let before = fixture
        .state
        .rooms
        .owner_left(&fixture.state.pool, owner_id, None)
        .await
        .unwrap();

    fixture.put(&owner, &room, &sha(b"abcd"), b"abcd").await;

    let after = fixture
        .state
        .rooms
        .owner_left(&fixture.state.pool, owner_id, None)
        .await
        .unwrap();
    assert_eq!(before - after, 4);
}

#[tokio::test]
async fn a_viewer_cannot_store_a_file() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user("owner@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    let viewer = fixture
        .member(&room, "viewer@example.org", Role::Viewer)
        .await;
    let (_, stranger) = fixture.user("stranger@example.org").await;

    let refused = fixture.put(&viewer, &room, &sha(b"x"), b"x").await;
    let hidden = fixture.put(&stranger, &room, &sha(b"x"), b"x").await;
    let unseen = fixture.get(&stranger, &room, &sha(b"x")).await;

    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    assert_eq!(unseen.status, StatusCode::NOT_FOUND);
    assert!(fixture.files().is_empty());
}

#[tokio::test]
async fn files_need_the_file_list_first() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;

    let put = fixture.put(&owner, &room, &sha(b"x"), b"x").await;
    let get = fixture.get(&owner, &room, &sha(b"x")).await;

    assert_eq!(put.status, StatusCode::CONFLICT);
    assert_eq!(get.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn deleting_the_workspace_deletes_its_files() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user("owner@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    fixture.put(&owner, &room, &sha(b"x"), b"x").await;
    assert_eq!(rows(&fixture).await, 1);

    let reply = fixture
        .request(
            Method::DELETE,
            &format!("/api/rooms/{room}"),
            &owner,
            None,
            Vec::new(),
        )
        .await;

    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert!(fixture.files().is_empty());
    assert!(!fixture.dir.join("blobs").join(&room).exists());
    assert_eq!(rows(&fixture).await, 0);
}
