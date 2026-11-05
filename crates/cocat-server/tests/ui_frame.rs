use std::collections::HashMap;
use std::fs;
use std::net::SocketAddr;
use std::time::SystemTime;

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use cocat_server::links::Links;
use cocat_server::mail::Mailer;
use cocat_server::ratelimit::LoginLimit;
use cocat_server::room::RoomManager;
use cocat_server::{AppState, auth, db, ws};
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "hunter2";
const PEER: &str = "203.0.113.7:41000";

/// Every POST the web UI answers, so the same-site table grows with each page.
const UI_POSTS: &[&str] = &[
    "/ui/login",
    "/ui/logout",
    "/ui/request-verify",
    "/ui/signup",
    "/ui/forgot-password",
    "/ui/reset-password",
    "/ui/verify",
    "/ui/account/password",
    "/ui/confirm-email",
    "/ui/rooms",
    "/ui/rooms/00000000000000000000000000000000/rename",
    "/ui/rooms/00000000000000000000000000000000/delete",
    "/ui/rooms/00000000000000000000000000000000/leave",
    "/ui/rooms/00000000000000000000000000000000/members/00000000000000000000000000000000/role",
    "/ui/rooms/00000000000000000000000000000000/members/00000000000000000000000000000000/remove",
    "/ui/rooms/00000000000000000000000000000000/members/00000000000000000000000000000000/owner",
    "/ui/rooms/00000000000000000000000000000000/invitations",
    "/ui/rooms/00000000000000000000000000000000/invitations/1/revoke",
];

struct Fixture {
    state: AppState,
    _dir: TempDir,
}

impl Fixture {
    async fn start() -> Self {
        Self::with_env(&[]).await
    }

    async fn with_env(env: &[(&str, &str)]) -> Self {
        let dir = TempDir::new().unwrap();
        let rooms = dir.path().join("rooms");
        fs::create_dir_all(&rooms).unwrap();
        let env: HashMap<String, String> = env
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        let lookup = |name: &str| env.get(name).cloned();
        let state = AppState {
            pool: db::open(&dir.path().join("test.db")).await.unwrap(),
            secret: "test-secret".to_owned(),
            rooms: RoomManager::new(rooms, None),
            before_attach: None,
            login_limit: LoginLimit::default(),
            mailer: Mailer::from_env(&lookup).unwrap(),
            links: Links::from_env(&lookup).unwrap(),
        };
        Self { state, _dir: dir }
    }

    async fn user(&self, email: &str) -> Uuid {
        db::create_user(&self.state.pool, email, PASSWORD, false, true)
            .await
            .unwrap()
    }

    fn token(&self, user: Uuid) -> String {
        auth::mint_token(user, &self.state.secret, SystemTime::now()).unwrap()
    }

    async fn get(&self, path: &str, token: Option<&str>) -> Response {
        self.send(Method::GET, path, token, &[], Body::empty())
            .await
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        headers: &[(&str, &str)],
        body: Body,
    ) -> Response {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            request = request.header(header::COOKIE, format!("fastapiusersauth={token}"));
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut request = request.body(body).unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(PEER.parse::<SocketAddr>().unwrap()));
        ws::router(self.state.clone())
            .oneshot(request)
            .await
            .unwrap()
    }
}

fn location(response: &Response) -> String {
    response
        .headers()
        .get(header::LOCATION)
        .expect("a redirect")
        .to_str()
        .unwrap()
        .to_owned()
}

async fn body_text(response: Response) -> String {
    let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn root_redirects_to_rooms() {
    let fixture = Fixture::start().await;

    let response = fixture.get("/ui/", None).await;

    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(location(&response), "/ui/rooms");
}

#[tokio::test]
async fn rooms_requires_login_with_next() {
    let fixture = Fixture::start().await;

    let response = fixture.get("/ui/rooms", None).await;

    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(location(&response), "/ui/login?next=%2Fui%2Frooms");
}

#[tokio::test]
async fn static_assets_served_with_cache_headers() {
    let fixture = Fixture::start().await;

    for (path, content_type, marker) in [
        ("/ui/static/style.css", "text/css", "body"),
        (
            "/ui/static/fragment.js",
            "text/javascript",
            "history.replaceState",
        ),
    ] {
        let response = fixture.get(path, None).await;
        let status = response.status();
        let headers = response.headers().clone();
        let body = body_text(response).await;

        assert_eq!(status, StatusCode::OK, "{path}");
        let supplied = headers[header::CONTENT_TYPE].to_str().unwrap();
        assert!(supplied.starts_with(content_type), "{path}: {supplied}");
        let cache = headers[header::CACHE_CONTROL].to_str().unwrap();
        assert!(cache.contains("max-age=31536000"), "{path}: {cache}");
        assert!(body.contains(marker), "{path} does not look like itself");
    }

    let missing = fixture.get("/ui/static/missing.css", None).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn prefix_applies_to_links_and_redirects() {
    let fixture = Fixture::with_env(&[("COCAT_PROXY_PREFIX", "/cocat")]).await;
    let user = fixture.user("member@example.org").await;
    let token = fixture.token(user);

    let rooms = fixture.get("/ui/rooms", Some(&token)).await;
    assert_eq!(rooms.status(), StatusCode::OK);
    let page = body_text(rooms).await;
    assert!(page.contains(r#"href="/cocat/ui/rooms""#), "{page}");
    assert!(page.contains(r#"href="/cocat/ui/account""#), "{page}");
    assert!(page.contains(r#"action="/cocat/ui/logout""#), "{page}");
    assert!(
        page.contains(r#"href="/cocat/ui/static/style.css""#),
        "{page}"
    );

    let root = fixture.get("/ui/", None).await;
    assert_eq!(location(&root), "/cocat/ui/rooms");

    let anonymous = fixture.get("/ui/rooms", None).await;
    assert_eq!(location(&anonymous), "/cocat/ui/login?next=%2Fui%2Frooms");
}

#[tokio::test]
async fn user_text_is_escaped() {
    let fixture = Fixture::start().await;
    let user = fixture
        .user(r#"<script>alert("x")</script>@example.org"#)
        .await;
    let token = fixture.token(user);

    let page = body_text(fixture.get("/ui/rooms", Some(&token)).await).await;

    assert!(!page.contains("<script>"), "{page}");
    assert!(page.contains("&#60;script&#62;"), "{page}");
}

#[tokio::test]
async fn cross_site_posts_are_refused() {
    let fixture = Fixture::start().await;

    for path in UI_POSTS {
        let foreign = fixture
            .send(
                Method::POST,
                path,
                None,
                &[("Host", "example.org"), ("Origin", "https://evil.example")],
                Body::empty(),
            )
            .await;
        assert_eq!(
            foreign.status(),
            StatusCode::FORBIDDEN,
            "{path} accepted a foreign Origin"
        );

        let cross = fixture
            .send(
                Method::POST,
                path,
                None,
                &[("Sec-Fetch-Site", "cross-site")],
                Body::empty(),
            )
            .await;
        assert_eq!(
            cross.status(),
            StatusCode::FORBIDDEN,
            "{path} accepted a cross-site post"
        );

        let same_origin = fixture
            .send(
                Method::POST,
                path,
                None,
                &[("Host", "example.org"), ("Origin", "https://example.org")],
                Body::empty(),
            )
            .await;
        assert_ne!(
            same_origin.status(),
            StatusCode::FORBIDDEN,
            "{path} refused its own origin"
        );

        let same_site = fixture
            .send(
                Method::POST,
                path,
                None,
                &[("Sec-Fetch-Site", "same-site")],
                Body::empty(),
            )
            .await;
        assert_ne!(
            same_site.status(),
            StatusCode::FORBIDDEN,
            "{path} refused a same-site post"
        );
    }
}
