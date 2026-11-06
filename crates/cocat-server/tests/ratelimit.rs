use std::fs;
use std::net::SocketAddr;
use std::process::Command;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderValue, Method, Request, StatusCode, header};
use axum::response::Response;
use cocat_server::ratelimit::{LoginLimit, RateLimiter, client_key};
use cocat_server::room::RoomManager;
use cocat_server::{AppState, auth, db, links::Links, ws};
use tempfile::TempDir;
use tower::ServiceExt;

const PEER: &str = "203.0.113.7:41000";

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
        };
        Self { state, _dir: dir }
    }

    async fn user(&self, email: &str) -> uuid::Uuid {
        db::create_user(&self.state.pool, email, "hunter2", false, true)
            .await
            .unwrap()
    }

    async fn token(&self, user: uuid::Uuid) -> String {
        auth::start_session(&self.state.pool, user).await.unwrap()
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        peer: &str,
        cookie: Option<String>,
        content_type: Option<&'static str>,
        body: Option<String>,
    ) -> Response {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        if let Some(content_type) = content_type {
            request = request.header(header::CONTENT_TYPE, content_type);
        }
        let mut request = request.body(Body::from(body.unwrap_or_default())).unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
        let router: Router = ws::router(self.state.clone());
        router.oneshot(request).await.unwrap()
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        peer: &str,
        cookie: Option<String>,
        content_type: Option<&'static str>,
        body: Option<String>,
    ) -> StatusCode {
        self.send(method, path, peer, cookie, content_type, body)
            .await
            .status()
    }

    async fn login(&self, peer: &str, email: &str) -> StatusCode {
        self.login_response(peer, email).await.0
    }

    async fn login_response(&self, peer: &str, email: &str) -> (StatusCode, String) {
        let response = self
            .send(
                Method::POST,
                "/auth/jwt/login",
                peer,
                None,
                Some("application/x-www-form-urlencoded"),
                Some(format!("username={email}&password=hunter2")),
            )
            .await;
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }
}

#[tokio::test]
async fn the_eleventh_login_in_a_minute_is_429() {
    let fixture = Fixture::start().await;
    fixture.user("guard@example.org").await;

    for attempt in 1..=10 {
        assert_eq!(
            fixture.login(PEER, "guard@example.org").await,
            StatusCode::NO_CONTENT,
            "login {attempt} was refused"
        );
    }

    let (status, body) = fixture.login_response(PEER, "guard@example.org").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body, r#"{"detail":"Too Many Requests"}"#);
}

#[tokio::test]
async fn the_window_resets_after_the_period() {
    let limiter = RateLimiter::new(2, Duration::from_secs(60), 100);
    let start = Instant::now();

    assert!(limiter.hit("key", start));
    assert!(limiter.hit("key", start + Duration::from_secs(59)));
    assert!(!limiter.hit("key", start + Duration::from_secs(59)));
    assert!(
        limiter.hit("key", start + Duration::from_secs(60)),
        "the window did not reset when the period elapsed"
    );
}

#[tokio::test]
async fn routes_have_separate_budgets() {
    let fixture = Fixture::start().await;
    let user = fixture.user("many@example.org").await;
    let cookie = format!("fastapiusersauth={}", fixture.token(user).await);

    for _ in 0..=10 {
        fixture.login(PEER, "many@example.org").await;
    }

    assert_eq!(
        fixture
            .call(Method::GET, "/api/me", PEER, Some(cookie), None, None)
            .await,
        StatusCode::OK,
        "a flood of logins locked the same client out of another route"
    );
}

#[test]
fn forwarded_for_is_ignored_without_trusted_proxies() {
    let headers = forwarded(&["198.51.100.1", "198.51.100.2"]);

    assert_eq!(
        client_key(&headers, Some(peer()), 0),
        "203.0.113.7",
        "a client spoofed its address through X-Forwarded-For"
    );
}

#[test]
fn the_trusted_hop_is_the_key() {
    let headers = forwarded(&["198.51.100.1", "198.51.100.2"]);

    assert_eq!(client_key(&headers, Some(peer()), 1), "198.51.100.2");
    assert_eq!(client_key(&headers, Some(peer()), 2), "198.51.100.1");
    assert_eq!(
        client_key(&headers, Some(peer()), 3),
        "203.0.113.7",
        "a header with fewer hops than trusted proxies must not be believed"
    );
}

#[test]
fn the_key_table_is_bounded() {
    let limiter = RateLimiter::new(1, Duration::from_secs(60), 2);
    let start = Instant::now();

    assert!(limiter.hit("a", start));
    assert!(!limiter.hit("a", start));
    assert!(limiter.hit("b", start + Duration::from_secs(1)));
    assert!(limiter.hit("c", start + Duration::from_secs(2)));

    assert!(
        limiter.hit("a", start + Duration::from_secs(2)),
        "the oldest window was not dropped to make room"
    );
}

#[test]
fn a_malformed_trusted_proxies_is_a_startup_error() {
    let dir = TempDir::new().unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cocat-server"))
        .args(["serve", "--port", "0", "--db_path"])
        .arg(dir.path().join("test.db"))
        .env("COCAT_TRUSTED_PROXIES", "two")
        .output()
        .unwrap();

    assert!(!output.status.success(), "serve accepted a bad proxy count");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("COCAT_TRUSTED_PROXIES"),
        "the error does not name the variable: {stderr}"
    );
}

fn peer() -> SocketAddr {
    PEER.parse().unwrap()
}

fn forwarded(hops: &[&str]) -> axum::http::HeaderMap {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "x-forwarded-for",
        HeaderValue::from_str(&hops.join(", ")).unwrap(),
    );
    headers
}
