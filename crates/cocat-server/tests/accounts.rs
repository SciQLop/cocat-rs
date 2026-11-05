use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use cocat_core::role::Role;
use cocat_server::links::Links;
use cocat_server::mail::Mailer;
use cocat_server::ratelimit::LoginLimit;
use cocat_server::room::RoomManager;
use cocat_server::{AppState, accounts, auth, db, ws};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "hunter2";
const PUBLIC_URL: &str = "https://cocat.example.org";
const VERIFY_BAD: &str = "VERIFY_USER_BAD_TOKEN";
const ALREADY_VERIFIED: &str = "VERIFY_USER_ALREADY_VERIFIED";
const RESET_BAD: &str = "RESET_PASSWORD_BAD_TOKEN";
const CLEARED_COOKIE: &str =
    r#"fastapiusersauth=""; HttpOnly; Max-Age=0; Path=/; SameSite=lax; Secure"#;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mail {
    Off,
    Directory,
    BrokenDirectory,
}

struct Fixture {
    state: AppState,
    mail_dir: Option<PathBuf>,
    _dir: TempDir,
}

impl Fixture {
    async fn start() -> Self {
        Self::build(Mail::Off).await
    }

    async fn start_with_mail() -> Self {
        Self::build(Mail::Directory).await
    }

    async fn start_with_broken_mail() -> Self {
        Self::build(Mail::BrokenDirectory).await
    }

    async fn build(mail: Mail) -> Self {
        let dir = TempDir::new().unwrap();
        let rooms = dir.path().join("rooms");
        fs::create_dir_all(&rooms).unwrap();
        let mail_dir = (mail != Mail::Off).then(|| {
            let name = match mail {
                Mail::Directory => "mail",
                _ => "missing",
            };
            dir.path().join(name)
        });
        if mail == Mail::Directory {
            fs::create_dir_all(mail_dir.as_ref().unwrap()).unwrap();
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

    async fn request_verify(&self, email: &str) -> Response {
        self.post_json("/auth/request-verify-token", json!({ "email": email }))
            .await
    }

    async fn verify(&self, token: &str) -> Response {
        self.post_json("/auth/verify", json!({ "token": token }))
            .await
    }

    async fn forgot(&self, email: &str) -> Response {
        self.post_json("/auth/forgot-password", json!({ "email": email }))
            .await
    }

    async fn reset(&self, token: &str, password: &str) -> Response {
        self.post_json(
            "/auth/reset-password",
            json!({ "token": token, "password": password }),
        )
        .await
    }

    fn mails(&self) -> Vec<String> {
        let Some(dir) = &self.mail_dir else {
            return Vec::new();
        };
        if !dir.is_dir() {
            return Vec::new();
        }
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

/// The token a mail carries after `marker`.
///
/// lettre writes the body as quoted-printable, so a long link arrives folded with soft line
/// breaks (`=\r\n`); they are undone before the token is read, and the token then ends at the
/// first whitespace.
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

/// A tracing writer that keeps every formatted event, so a test can assert what was *not*
/// recorded. `tracing::subscriber::set_default` scopes it to the test's own thread.
mod capture {
    use std::io;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    pub struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Capture {
        pub fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().expect("the log")).into_owned()
        }
    }

    impl io::Write for Capture {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("the log").extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Capture;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }
}

#[tokio::test]
async fn verify_flow() {
    let fixture = Fixture::start_with_mail().await;
    let registered = fixture.register("new@example.org").await;
    assert_eq!(registered.status(), StatusCode::CREATED);
    let token = link_token(&fixture.mails(), "/ui/verify#");

    let response = fixture.verify(&token).await;

    assert_eq!(response.status(), StatusCode::OK);
    let user = body(response).await;
    assert_eq!(user["email"], "new@example.org");
    assert_eq!(user["is_verified"], true);
    let logged_in = fixture.login("new@example.org", PASSWORD).await;
    assert_eq!(logged_in.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn verify_twice_is_already_verified() {
    let fixture = Fixture::start_with_mail().await;
    fixture.register("new@example.org").await;
    let token = link_token(&fixture.mails(), "/ui/verify#");
    assert_eq!(fixture.verify(&token).await.status(), StatusCode::OK);

    let again = fixture.verify(&token).await;

    assert_eq!(again.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body(again).await, json!({ "detail": ALREADY_VERIFIED }));
}

#[tokio::test]
async fn a_verify_token_for_an_old_email_is_bad() {
    let fixture = Fixture::start_with_mail().await;
    let id = fixture.unverified("new@example.org").await;
    let token = accounts::verification_token(
        id,
        "old@example.org",
        &fixture.state.secret,
        SystemTime::now(),
    )
    .unwrap();

    let response = fixture.verify(&token).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body(response).await, json!({ "detail": VERIFY_BAD }));
}

#[tokio::test]
async fn a_request_verify_token_mails_again_for_an_unverified_account() {
    let fixture = Fixture::start_with_mail().await;
    let id = fixture.unverified("new@example.org").await;

    let response = fixture.request_verify("new@example.org").await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let mails = fixture.mails();
    assert_eq!(mails.len(), 1, "{mails:?}");
    let token = link_token(&mails, "/ui/verify#");
    assert_eq!(fixture.verify(&token).await.status(), StatusCode::OK);
    assert!(
        db::user_by_id(fixture.pool(), id)
            .await
            .unwrap()
            .unwrap()
            .is_verified
    );
}

#[tokio::test]
async fn forgot_password_answers_202_for_anyone() {
    let fixture = Fixture::start_with_mail().await;
    fixture.verified("known@example.org").await;

    for address in [
        "known@example.org",
        "unknown@example.org",
        "orcid-1@orcid.placeholder.cocat.org",
    ] {
        let response = fixture.forgot(address).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED, "{address}");
    }

    let mails = fixture.mails();
    assert_eq!(
        mails.len(),
        1,
        "only the known address is mailed: {mails:?}"
    );
    assert!(mails[0].contains("To: known@example.org"), "{}", mails[0]);
    assert!(
        mails[0].contains("Subject: Reset your cocat password"),
        "{}",
        mails[0]
    );
}

#[tokio::test]
async fn reset_flow() {
    let fixture = Fixture::start_with_mail().await;
    fixture.verified("known@example.org").await;
    fixture.forgot("known@example.org").await;
    let token = link_token(&fixture.mails(), "/ui/reset-password#");

    let response = fixture.reset(&token, "new-password").await;

    assert_eq!(response.status(), StatusCode::OK);
    let logged_in = fixture.login("known@example.org", "new-password").await;
    assert_eq!(logged_in.status(), StatusCode::NO_CONTENT);
    let old = fixture.login("known@example.org", PASSWORD).await;
    assert_eq!(old.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_reset_token_works_once() {
    let fixture = Fixture::start_with_mail().await;
    fixture.verified("known@example.org").await;
    fixture.forgot("known@example.org").await;
    let token = link_token(&fixture.mails(), "/ui/reset-password#");
    assert_eq!(
        fixture.reset(&token, "new-password").await.status(),
        StatusCode::OK
    );

    let again = fixture.reset(&token, "another-password").await;

    assert_eq!(again.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body(again).await, json!({ "detail": RESET_BAD }));
    assert_eq!(
        fixture
            .login("known@example.org", "new-password")
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
}

/// Two requests can read the same row before either writes. The link is spent by the
/// password change, so the conditional update is what makes exactly one of them win.
#[tokio::test]
async fn two_resets_of_one_link_cannot_both_win() {
    let fixture = Fixture::start_with_mail().await;
    fixture.verified("known@example.org").await;
    fixture.forgot("known@example.org").await;
    let token = link_token(&fixture.mails(), "/ui/reset-password#");

    let (first, second) = tokio::join!(
        fixture.reset(&token, "first-password"),
        fixture.reset(&token, "second-password"),
    );

    let statuses = [first.status(), second.status()];
    let wins = statuses.iter().filter(|s| **s == StatusCode::OK).count();
    assert_eq!(wins, 1, "one link changes one password: {statuses:?}");
}

#[tokio::test]
async fn an_expired_token_is_bad() {
    let fixture = Fixture::start_with_mail().await;
    let past = SystemTime::now() - Duration::from_secs(2 * 3600);
    let pending = fixture.unverified("new@example.org").await;
    let verify =
        accounts::verification_token(pending, "new@example.org", &fixture.state.secret, past)
            .unwrap();

    let response = fixture.verify(&verify).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body(response).await, json!({ "detail": VERIFY_BAD }));

    let known = fixture.verified("known@example.org").await;
    let user = db::user_by_id(fixture.pool(), known)
        .await
        .unwrap()
        .unwrap();
    let reset =
        accounts::reset_token(known, &user.hashed_password, &fixture.state.secret, past).unwrap();

    let response = fixture.reset(&reset, "new-password").await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body(response).await, json!({ "detail": RESET_BAD }));
}

#[tokio::test]
async fn tokens_are_not_interchangeable() {
    let fixture = Fixture::start_with_mail().await;
    let pending = fixture.unverified("new@example.org").await;
    let user = db::user_by_id(fixture.pool(), pending)
        .await
        .unwrap()
        .unwrap();
    let now = SystemTime::now();
    let verify =
        accounts::verification_token(pending, "new@example.org", &fixture.state.secret, now)
            .unwrap();
    let reset =
        accounts::reset_token(pending, &user.hashed_password, &fixture.state.secret, now).unwrap();

    let as_reset = fixture.reset(&verify, "new-password").await;
    assert_eq!(as_reset.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body(as_reset).await, json!({ "detail": RESET_BAD }));

    let as_verify = fixture.verify(&reset).await;
    assert_eq!(as_verify.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body(as_verify).await, json!({ "detail": VERIFY_BAD }));

    fixture.verified("known@example.org").await;
    let session = cookie_token(&fixture.login("known@example.org", PASSWORD).await);
    assert_eq!(
        fixture.verify(&session).await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        fixture.reset(&session, "new-password").await.status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn no_token_or_link_reaches_the_log() {
    let fixture = Fixture::start_with_broken_mail().await;
    let log = capture::Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let pending = fixture.unverified("new@example.org").await;
    let token = accounts::verification_token(
        pending,
        "new@example.org",
        &fixture.state.secret,
        SystemTime::now(),
    )
    .unwrap();
    assert_eq!(fixture.verify(&token).await.status(), StatusCode::OK);
    fixture.verified("known@example.org").await;
    assert_eq!(
        fixture.forgot("known@example.org").await.status(),
        StatusCode::ACCEPTED
    );

    let text = log.text();
    assert!(
        !text.is_empty(),
        "nothing was captured, the test proves nothing"
    );
    assert!(!text.contains(&token), "a token reached the log: {text}");
    assert!(!text.contains("eyJ"), "a token reached the log: {text}");
    assert!(!text.contains(PUBLIC_URL), "a link reached the log: {text}");
}

#[tokio::test]
async fn forgot_password_is_rate_limited() {
    let fixture = Fixture::start_with_mail().await;
    fixture.verified("known@example.org").await;

    for _ in 0..10 {
        let response = fixture.forgot("known@example.org").await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    let refused = fixture.forgot("known@example.org").await;

    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        body(refused).await,
        json!({ "detail": "Too Many Requests" })
    );
}
