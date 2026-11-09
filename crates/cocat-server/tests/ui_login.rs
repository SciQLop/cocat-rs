use std::collections::HashMap;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use cocat_server::links::Links;
use cocat_server::mail::Mailer;
use cocat_server::ratelimit::LoginLimit;
use cocat_server::room::RoomManager;
use cocat_server::{AppState, accounts, auth, db, ws};
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "plum orbit lantern cello";
const PEER: &str = "203.0.113.7:41000";
const PUBLIC_URL: &str = "https://cocat.example.org";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mail {
    Off,
    Directory,
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

    async fn build(mail: Mail) -> Self {
        let dir = TempDir::new().unwrap();
        let rooms = dir.path().join("rooms");
        fs::create_dir_all(&rooms).unwrap();
        let mail_dir = (mail == Mail::Directory).then(|| dir.path().join("mail"));
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
            oidc: Vec::new(),
            web_dir: None,
        };
        Self {
            state,
            mail_dir,
            _dir: dir,
        }
    }

    async fn verified(&self, email: &str) -> Uuid {
        db::create_user(&self.state.pool, email, PASSWORD, false, true)
            .await
            .unwrap()
    }

    async fn unverified(&self, email: &str) -> Uuid {
        db::create_user(&self.state.pool, email, PASSWORD, false, false)
            .await
            .unwrap()
    }

    async fn deactivate(&self, email: &str) {
        let user = db::user_by_email(&self.state.pool, email)
            .await
            .unwrap()
            .expect("the account exists");
        accounts::change(
            &self.state,
            user,
            accounts::Change {
                is_active: Some(false),
                ..accounts::Change::default()
            },
        )
        .await
        .unwrap();
    }

    async fn token(&self, user: Uuid) -> String {
        auth::start_session(&self.state.pool, user).await.unwrap()
    }

    async fn get(&self, path: &str, token: Option<&str>) -> Response {
        self.send(Method::GET, path, token, None, Body::empty())
            .await
    }

    async fn post(&self, path: &str, token: Option<&str>, form: &str) -> Response {
        self.send(Method::POST, path, token, Some(form), Body::empty())
            .await
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        form: Option<&str>,
        body: Body,
    ) -> Response {
        self.send_with(method, path, token, &[], form, body).await
    }

    async fn send_with(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        headers: &[(&str, &str)],
        form: Option<&str>,
        body: Body,
    ) -> Response {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            request = request.header(header::COOKIE, format!("fastapiusersauth={token}"));
        }
        if form.is_some() {
            request = request.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut request = request
            .body(match form {
                Some(form) => Body::from(form.to_owned()),
                None => body,
            })
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(PEER.parse::<SocketAddr>().unwrap()));
        ws::router(self.state.clone())
            .oneshot(request)
            .await
            .unwrap()
    }

    async fn login(&self, email: &str, password: &str, next: Option<&str>) -> Response {
        let path = match next {
            Some(next) => format!("/ui/login?next={}", encode(next)),
            None => "/ui/login".to_owned(),
        };
        let form = format!("username={}&password={}", encode(email), encode(password));
        self.post(&path, None, &form).await
    }

    fn with_prefix(mut self, prefix: &str) -> Self {
        let prefix = prefix.to_owned();
        self.state.links =
            Links::from_env(&move |name| (name == "COCAT_PROXY_PREFIX").then(|| prefix.clone()))
                .unwrap();
        self
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

    /// A spawned send writes a file; polling the directory is the only way to know it
    /// landed, so the wait is bounded rather than a synchronisation sleep.
    async fn wait_for_mail(&self, to: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let marker = format!("To: {to}");
            if let Some(mail) = self.mails().into_iter().find(|mail| mail.contains(&marker)) {
                return mail;
            }
            assert!(Instant::now() < deadline, "no mail to {to}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

fn encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            b' ' => encoded.push('+'),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
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

fn set_cookie(response: &Response) -> Option<String> {
    response
        .headers()
        .get(header::SET_COOKIE)
        .map(|value| value.to_str().unwrap().to_owned())
}

async fn body_text(response: Response) -> String {
    let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn form_posts_the_password() {
    let fixture = Fixture::start().await;

    let page = body_text(fixture.get("/ui/login?next=%2Fui%2Faccount", None).await).await;

    assert!(page.contains("Sign in"), "{page}");
    assert!(
        page.contains(r#"method="post" action="/ui/login?next=%2Fui%2Faccount""#),
        "{page}"
    );
    assert!(page.contains(r#"name="username""#), "{page}");
    assert!(page.contains(r#"name="password""#), "{page}");
    assert!(page.contains(r#"type="password""#), "{page}");
    assert!(page.contains("Log in"), "{page}");
    assert!(page.contains(r#"href="/ui/signup""#), "{page}");
    assert!(page.contains(r#"href="/ui/forgot-password""#), "{page}");
    assert!(
        page.contains(r#"method="post" action="/ui/request-verify""#),
        "{page}"
    );
    assert!(!page.contains("Sign in with"), "{page}");
}

#[tokio::test]
async fn login_page_shows_the_message() {
    let fixture = Fixture::start().await;

    let page = body_text(fixture.get("/ui/login?error=credentials", None).await).await;

    assert!(page.contains("Wrong email or password."), "{page}");
}

#[tokio::test]
async fn login_page_redirects_a_signed_in_visitor() {
    let fixture = Fixture::start().await;
    let user = fixture.verified("member@example.org").await;
    let token = fixture.token(user).await;

    let response = fixture.get("/ui/login", Some(&token)).await;

    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(location(&response), "/ui/app");
}

#[tokio::test]
async fn login_page_sends_a_signed_in_visitor_to_their_next() {
    let fixture = Fixture::start().await;
    let user = fixture.verified("member@example.org").await;
    let token = fixture.token(user).await;

    let local = fixture
        .get("/ui/login?next=%2Fui%2Faccount", Some(&token))
        .await;
    let foreign = fixture
        .get("/ui/login?next=https%3A%2F%2Fevil.com", Some(&token))
        .await;

    assert_eq!(location(&local), "/ui/account");
    assert_eq!(location(&foreign), "/ui/app");
}

#[tokio::test]
async fn login_sets_cookie_and_redirects() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;

    let response = fixture.login("member@example.org", PASSWORD, None).await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/ui/app");
    let cookie = set_cookie(&response).expect("a session cookie");
    assert!(cookie.starts_with("fastapiusersauth="), "{cookie}");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
}

#[tokio::test]
async fn default_next_is_the_app() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;

    let response = fixture.login("member@example.org", PASSWORD, None).await;

    assert_eq!(location(&response), "/ui/app");
}

#[tokio::test]
async fn bad_password() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;

    let response = fixture.login("member@example.org", "wrong", None).await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/ui/login?error=credentials");
    assert!(set_cookie(&response).is_none(), "no session was handed out");
}

#[tokio::test]
async fn inactive_user_refused() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;
    fixture.deactivate("member@example.org").await;

    let response = fixture.login("member@example.org", PASSWORD, None).await;

    assert_eq!(location(&response), "/ui/login?error=credentials");
    assert!(set_cookie(&response).is_none());
}

#[tokio::test]
async fn unverified_with_mail_refused() {
    let fixture = Fixture::start_with_mail().await;
    fixture.unverified("pending@example.org").await;

    let response = fixture.login("pending@example.org", PASSWORD, None).await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/ui/login?error=unverified");
    assert!(set_cookie(&response).is_none());
}

#[tokio::test]
async fn local_next_kept_with_query() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;

    let account = fixture
        .login("member@example.org", PASSWORD, Some("/ui/account"))
        .await;
    let query = fixture
        .login("member@example.org", PASSWORD, Some("/ui/rooms?x=1"))
        .await;

    assert_eq!(location(&account), "/ui/account");
    assert_eq!(location(&query), "/ui/rooms?x=1");
}

#[tokio::test]
async fn foreign_next_refused() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;

    for next in [
        "//evil.com",
        "/\\evil.com",
        "https://evil.com",
        "javascript:alert(1)",
        "/\t/evil.com",
        "/\n/evil.com",
    ] {
        let response = fixture
            .login("member@example.org", PASSWORD, Some(next))
            .await;
        assert_eq!(location(&response), "/ui/app", "{next:?} was followed");
    }
}

/// Percent-encoded bytes are not decoded by a browser resolving a `Location`, so these
/// stay paths on this origin; what is returned keeps them encoded, so the header never
/// carries a raw control character or a leading slash pair.
#[tokio::test]
async fn encoded_slashes_and_tabs_stay_a_path() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;

    for next in ["/%2F%2Fevil.com", "/%09/evil.com"] {
        let response = fixture
            .login("member@example.org", PASSWORD, Some(next))
            .await;
        assert_eq!(location(&response), next, "{next:?} was not kept");
    }
}

#[tokio::test]
async fn raw_newline_next_redirects_cleanly() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;

    let response = fixture
        .login("member@example.org", PASSWORD, Some("/\r\n/evil.com"))
        .await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/ui/app");
}

#[tokio::test]
async fn login_rate_limited() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;

    for attempt in 1..=10 {
        let response = fixture.login("member@example.org", "wrong", None).await;
        assert_eq!(
            location(&response),
            "/ui/login?error=credentials",
            "attempt {attempt}"
        );
    }

    let refused = fixture.login("member@example.org", "wrong", None).await;

    assert_eq!(refused.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&refused), "/ui/login?error=too_many");
}

#[tokio::test]
async fn cross_site_login_has_no_side_effect() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;

    let response = fixture
        .send_with(
            Method::POST,
            "/ui/login",
            None,
            &[("Host", "example.org"), ("Origin", "https://evil.example")],
            Some(&format!("username=member@example.org&password={PASSWORD}")),
            Body::empty(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(set_cookie(&response).is_none());
}

#[tokio::test]
async fn logout_clears_cookie() {
    let fixture = Fixture::start().await;
    let user = fixture.verified("member@example.org").await;
    let token = fixture.token(user).await;

    let response = fixture.post("/ui/logout", Some(&token), "").await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/ui/login");
    let cookie = set_cookie(&response).expect("the cookie is cleared");
    assert!(cookie.contains("Max-Age=0"), "{cookie}");
}

#[tokio::test]
async fn logout_without_a_cookie() {
    let fixture = Fixture::start().await;

    let response = fixture.post("/ui/logout", None, "").await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/ui/login");
}

#[tokio::test]
async fn logout_not_rate_limited() {
    let fixture = Fixture::start().await;
    let user = fixture.verified("member@example.org").await;
    let token = fixture.token(user).await;
    for _ in 0..10 {
        fixture.login("member@example.org", "wrong", None).await;
    }

    let response = fixture.post("/ui/logout", Some(&token), "").await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/ui/login");
}

#[tokio::test]
async fn resend_answers_the_same_for_unknown_and_known() {
    let fixture = Fixture::start_with_mail().await;
    fixture.verified("known@example.org").await;

    let unknown = fixture
        .post("/ui/request-verify", None, "email=nobody%40example.org")
        .await;
    let known = fixture
        .post("/ui/request-verify", None, "email=known%40example.org")
        .await;

    for response in [unknown, known] {
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(location(&response), "/ui/login?resent=1");
    }
    assert!(
        fixture.mails().is_empty(),
        "a verified account is not mailed"
    );
}

#[tokio::test]
async fn resend_mails_the_unverified() {
    let fixture = Fixture::start_with_mail().await;
    fixture.unverified("pending@example.org").await;

    let response = fixture
        .post("/ui/request-verify", None, "email=pending%40example.org")
        .await;

    assert_eq!(location(&response), "/ui/login?resent=1");
    let mail = fixture.wait_for_mail("pending@example.org").await;
    assert!(
        mail.contains("Subject: Confirm your cocat address"),
        "{mail}"
    );
}

#[tokio::test]
async fn login_flood_does_not_block_signup() {
    let fixture = Fixture::start().await;
    for _ in 0..10 {
        fixture.login("member@example.org", "wrong", None).await;
    }

    let response = fixture
        .post(
            "/ui/signup",
            None,
            "email=fresh%40example.org&password=plum+orbit+lantern+cello",
        )
        .await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/ui/app");
}

#[tokio::test]
async fn next_keeps_the_proxy_prefix() {
    let fixture = Fixture::start().await.with_prefix("/cocat-dev");
    fixture.verified("member@example.org").await;

    let response = fixture
        .login("member@example.org", PASSWORD, Some("/ui/rooms"))
        .await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/cocat-dev/ui/rooms");
}

#[tokio::test]
async fn next_carrying_the_prefix_is_not_doubled() {
    let fixture = Fixture::start().await.with_prefix("/cocat-dev");
    fixture.verified("member@example.org").await;

    let response = fixture
        .login(
            "member@example.org",
            PASSWORD,
            Some("/cocat-dev/ui/rooms?tag=a#top"),
        )
        .await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/cocat-dev/ui/rooms?tag=a#top");
}

#[tokio::test]
async fn off_site_next_falls_back_inside_the_prefix() {
    let fixture = Fixture::start().await.with_prefix("/cocat-dev");
    fixture.verified("member@example.org").await;

    let response = fixture
        .login(
            "member@example.org",
            PASSWORD,
            Some("https://evil.example/x"),
        )
        .await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/cocat-dev/ui/app");
}
