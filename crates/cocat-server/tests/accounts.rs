use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use cocat_core::role::Role;
use cocat_server::links::Links;
use cocat_server::mail::Mailer;
use cocat_server::ratelimit::LoginLimit;
use cocat_server::room::RoomManager;
use cocat_server::{AppState, auth, db, ws};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "hunter2";
const PUBLIC_URL: &str = "https://cocat.example.org";
const CLEARED_COOKIE: &str =
    r#"fastapiusersauth=""; HttpOnly; Max-Age=0; Path=/; SameSite=lax; Secure"#;

struct Fixture {
    state: AppState,
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
        if let Some(path) = &mail_dir {
            fs::create_dir_all(path).unwrap();
        }
        let mut env: HashMap<&str, String> = HashMap::new();
        if let Some(path) = &mail_dir {
            env.insert("COCAT_MAIL_DIR", path.to_string_lossy().into_owned());
            env.insert("COCAT_MAIL_FROM", "cocat@example.org".to_owned());
            env.insert("COCAT_PUBLIC_URL", PUBLIC_URL.to_owned());
        }
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
        Self {
            state,
            mail_dir,
            _dir: dir,
        }
    }

    fn pool(&self) -> &SqlitePool {
        &self.state.pool
    }

    async fn send(&self, request: Request<Body>) -> Response {
        ws::router(self.state.clone())
            .oneshot(request)
            .await
            .unwrap()
    }

    async fn post_json(&self, path: &str, body: Value) -> Response {
        let request = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        self.send(request).await
    }

    async fn post_form(&self, path: &str, form: &str, token: Option<&str>) -> Response {
        let mut request = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        if let Some(token) = token {
            request = request.header(header::COOKIE, format!("fastapiusersauth={token}"));
        }
        self.send(request.body(Body::from(form.to_owned())).unwrap())
            .await
    }

    async fn register(&self, email: &str) -> Response {
        self.post_json(
            "/auth/register",
            json!({ "email": email, "password": PASSWORD }),
        )
        .await
    }

    async fn login(&self, email: &str, password: &str) -> Response {
        let form = format!("username={email}&password={password}");
        self.post_form("/auth/jwt/login", &form, None).await
    }

    async fn unverified(&self, email: &str) -> Uuid {
        let id = db::create_user(self.pool(), email, PASSWORD, false, false)
            .await
            .unwrap();
        db::create_personal_room(self.pool(), id).await.unwrap();
        id
    }

    async fn verified(&self, email: &str) -> Uuid {
        db::create_user(self.pool(), email, PASSWORD, false, true)
            .await
            .unwrap()
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
}

async fn body(response: Response) -> Value {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    if bytes.is_empty() {
        assert_eq!(status, StatusCode::NO_CONTENT, "an empty body is a 204");
        return Value::Null;
    }
    serde_json::from_slice(&bytes).expect("the response is not JSON")
}

fn set_cookie(response: &Response) -> Option<String> {
    response
        .headers()
        .get(header::SET_COOKIE)
        .map(|value| value.to_str().unwrap().to_owned())
}

fn cookie_token(response: &Response) -> String {
    let cookie = set_cookie(response).expect("a session cookie");
    let (name, value) = cookie.split_once('=').expect("a cookie pair");
    assert_eq!(name, "fastapiusersauth");
    value.split(';').next().unwrap().to_owned()
}

#[tokio::test]
async fn register_creates_a_verified_account_without_a_mailer() {
    let fixture = Fixture::start().await;

    let response = fixture.register("new@example.org").await;

    assert_eq!(response.status(), StatusCode::CREATED);
    assert!(set_cookie(&response).is_none(), "register does not log in");
    let user = body(response).await;
    assert_eq!(user["email"], "new@example.org");
    assert_eq!(user["is_active"], true);
    assert_eq!(user["is_superuser"], false);
    assert_eq!(user["is_verified"], true);
    let id = user["id"].as_str().expect("an id");
    assert!(Uuid::parse_str(id).is_ok(), "{id} is not a uuid");
}

#[tokio::test]
async fn register_with_a_mailer_sends_a_verification_and_leaves_it_unverified() {
    let fixture = Fixture::start_with_mail().await;

    let response = fixture.register("new@example.org").await;

    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(body(response).await["is_verified"], false);
    let mails = fixture.mails();
    assert_eq!(mails.len(), 1, "{mails:?}");
    assert!(mails[0].contains("To: new@example.org"), "{}", mails[0]);
    assert!(
        mails[0].contains("Subject: Confirm your cocat address"),
        "{}",
        mails[0]
    );
    let flat: String = mails[0].split_whitespace().collect();
    assert!(
        flat.contains(&format!("{PUBLIC_URL}/ui/verify#")),
        "the mail does not carry the link: {}",
        mails[0]
    );
}

#[tokio::test]
async fn register_creates_the_personal_room() {
    let fixture = Fixture::start().await;

    let user = body(fixture.register("new@example.org").await).await;
    let id = Uuid::parse_str(user["id"].as_str().unwrap()).unwrap();

    let rooms = db::list_user_rooms(fixture.pool(), id).await.unwrap();
    assert_eq!(rooms.len(), 1, "{rooms:?}");
    assert_eq!(rooms[0].0.name, db::PERSONAL_ROOM_NAME);
    assert_eq!(rooms[0].1, Role::Owner);
}

#[tokio::test]
async fn a_duplicate_email_any_case_is_refused() {
    let fixture = Fixture::start().await;
    let first = fixture.register("Dup@Example.org").await;
    assert_eq!(first.status(), StatusCode::CREATED);

    let second = fixture.register("dup@example.org").await;

    assert_eq!(second.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body(second).await,
        json!({ "detail": "REGISTER_USER_ALREADY_EXISTS" })
    );
}

#[tokio::test]
async fn a_placeholder_address_cannot_register() {
    let fixture = Fixture::start().await;

    for address in [
        "orcid-1@orcid.placeholder.cocat.org",
        "not-an-address",
        "a@b",
    ] {
        let response = fixture.register(address).await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{address} was accepted"
        );
    }
    let stored = db::user_by_email(fixture.pool(), "a@b").await.unwrap();
    assert!(stored.is_none(), "a placeholder address created no account");
}

#[tokio::test]
async fn without_a_mailer_an_unverified_account_logs_in() {
    let fixture = Fixture::start().await;
    fixture.unverified("pending@example.org").await;

    let response = fixture.login("pending@example.org", PASSWORD).await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(!cookie_token(&response).is_empty());
}

#[tokio::test]
async fn with_a_mailer_an_unverified_account_is_refused() {
    let fixture = Fixture::start_with_mail().await;
    fixture.unverified("pending@example.org").await;

    let response = fixture.login("pending@example.org", PASSWORD).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(set_cookie(&response).is_none(), "no session was handed out");
    assert_eq!(
        body(response).await,
        json!({ "detail": "LOGIN_USER_NOT_VERIFIED" })
    );
}

#[tokio::test]
async fn a_verified_account_logs_in_with_a_mailer() {
    let fixture = Fixture::start_with_mail().await;
    fixture.verified("known@example.org").await;

    let response = fixture.login("known@example.org", PASSWORD).await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(!cookie_token(&response).is_empty());
}

#[tokio::test]
async fn a_wrong_password_on_an_unverified_account_is_bad_credentials() {
    let fixture = Fixture::start_with_mail().await;
    fixture.unverified("pending@example.org").await;

    let wrong = fixture.login("pending@example.org", "wrong").await;
    let unknown = fixture.login("nobody@example.org", PASSWORD).await;

    for response in [wrong, unknown] {
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body(response).await,
            json!({ "detail": "LOGIN_BAD_CREDENTIALS" })
        );
    }
}

#[tokio::test]
async fn logout_clears_the_cookie() {
    let fixture = Fixture::start().await;
    fixture.verified("known@example.org").await;
    let token = cookie_token(&fixture.login("known@example.org", PASSWORD).await);

    let response = fixture
        .post_form("/auth/jwt/logout", "", Some(&token))
        .await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(set_cookie(&response).as_deref(), Some(CLEARED_COOKIE));

    let anonymous = fixture.post_form("/auth/jwt/logout", "", None).await;
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

    let forged = fixture
        .post_form("/auth/jwt/logout", "", Some("nonsense"))
        .await;
    assert_eq!(forged.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn with_a_mailer_an_unverified_cookie_cannot_log_out() {
    let fixture = Fixture::start_with_mail().await;
    let id = fixture.unverified("pending@example.org").await;
    let token = auth::mint_token(id, &fixture.state.secret, SystemTime::now()).unwrap();

    let response = fixture
        .post_form("/auth/jwt/logout", "", Some(&token))
        .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn register_is_rate_limited() {
    let fixture = Fixture::start().await;

    for index in 0..10 {
        let response = fixture.register(&format!("new{index}@example.org")).await;
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    let refused = fixture.register("late@example.org").await;

    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        body(refused).await,
        json!({ "detail": "Too Many Requests" })
    );
}
