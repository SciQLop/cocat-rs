//! The fixture the `ui_*` page tests share: a server, a mail sink, and the request helpers.
#![allow(dead_code)] // each test binary reaches for a different part of this fixture.

use std::collections::HashMap;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, header};
use axum::response::Response;
use cocat_server::links::Links;
use cocat_server::mail::Mailer;
use cocat_server::ratelimit::LoginLimit;
use cocat_server::room::RoomManager;
use cocat_server::{AppState, accounts, auth, db, ws};
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

pub const PASSWORD: &str = "hunter2";
pub const PUBLIC_URL: &str = "https://cocat.example.org";
const PEER: &str = "203.0.113.7:41000";

/// How the fixture reaches the outside world: not at all, a directory of `.eml` files, or a
/// socket that accepts a connection and never greets, so a send that is awaited hangs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mail {
    Off,
    Directory,
    Silent,
}

pub struct Fixture {
    pub state: AppState,
    mail_dir: Option<PathBuf>,
    _dir: TempDir,
}

impl Fixture {
    pub async fn start() -> Self {
        Self::build(Mail::Off).await
    }

    pub async fn start_with_mail() -> Self {
        Self::build(Mail::Directory).await
    }

    /// The mailer cannot finish, so any route that waits for it shows up as a slow answer.
    pub async fn start_with_slow_mail() -> Self {
        Self::build(Mail::Silent).await
    }

    async fn build(mail: Mail) -> Self {
        let dir = TempDir::new().unwrap();
        let rooms = dir.path().join("rooms");
        fs::create_dir_all(&rooms).unwrap();
        let mut env: HashMap<&str, String> = HashMap::new();
        let mail_dir = match mail {
            Mail::Off => None,
            Mail::Directory => {
                let path = dir.path().join("mail");
                fs::create_dir_all(&path).unwrap();
                env.insert("COCAT_MAIL_DIR", path.to_string_lossy().into_owned());
                Some(path)
            }
            Mail::Silent => {
                env.insert("COCAT_SMTP_HOST", "127.0.0.1".to_owned());
                env.insert("COCAT_SMTP_PORT", silent_port().await.to_string());
                None
            }
        };
        if mail != Mail::Off {
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

    pub async fn verified(&self, email: &str) -> Uuid {
        db::create_user(&self.state.pool, email, PASSWORD, false, true)
            .await
            .unwrap()
    }

    pub async fn unverified(&self, email: &str) -> Uuid {
        db::create_user(&self.state.pool, email, PASSWORD, false, false)
            .await
            .unwrap()
    }

    pub async fn deactivate(&self, email: &str) {
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

    pub fn token(&self, user: Uuid) -> String {
        auth::mint_token(user, &self.state.secret, SystemTime::now()).unwrap()
    }

    /// Remove the sink, so the next send fails the way a refusing relay would.
    pub fn break_mail(&self) {
        if let Some(dir) = &self.mail_dir {
            fs::remove_dir_all(dir).unwrap();
        }
    }

    pub async fn get(&self, path: &str, token: Option<&str>) -> Response {
        self.send(Method::GET, path, token, &[], None).await
    }

    /// A GET that carries a cookie the previous response set, such as the flash.
    pub async fn get_with(&self, path: &str, token: &str, cookie: &str) -> Response {
        self.send(Method::GET, path, Some(token), &[cookie], None)
            .await
    }

    pub async fn post(&self, path: &str, token: Option<&str>, form: &str) -> Response {
        self.send(Method::POST, path, token, &[], Some(form)).await
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        cookies: &[&str],
        form: Option<&str>,
    ) -> Response {
        let mut request = Request::builder().method(method).uri(path);
        let mut jar: Vec<String> = token
            .map(|token| format!("fastapiusersauth={token}"))
            .into_iter()
            .collect();
        jar.extend(cookies.iter().map(|cookie| (*cookie).to_owned()));
        if !jar.is_empty() {
            request = request.header(header::COOKIE, jar.join("; "));
        }
        if form.is_some() {
            request = request.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        }
        let mut request = request
            .body(match form {
                Some(form) => Body::from(form.to_owned()),
                None => Body::empty(),
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

    pub async fn sign_up(&self, email: &str, password: &str) -> Response {
        let form = form(&[("email", email), ("password", password)]);
        self.post("/ui/signup", None, &form).await
    }

    pub async fn forgot(&self, email: &str) -> Response {
        let form = form(&[("email", email)]);
        self.post("/ui/forgot-password", None, &form).await
    }

    pub async fn login(&self, email: &str, password: &str, next: Option<&str>) -> Response {
        let path = match next {
            Some(next) => format!("/ui/login?next={}", encode(next)),
            None => "/ui/login".to_owned(),
        };
        let form = form(&[("username", email), ("password", password)]);
        self.post(&path, None, &form).await
    }

    pub fn mails(&self) -> Vec<String> {
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
    pub async fn wait_for_mail(&self, to: &str) -> String {
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

/// A listener that accepts and holds every connection, so an SMTP client waits for a greeting
/// that never comes.
async fn silent_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(30)).await;
                drop(socket);
            });
        }
    });
    port
}

pub fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(name, value)| format!("{}={}", encode(name), encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

pub fn encode(value: &str) -> String {
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

pub fn location(response: &Response) -> String {
    response
        .headers()
        .get(header::LOCATION)
        .expect("a redirect")
        .to_str()
        .unwrap()
        .to_owned()
}

pub fn set_cookie(response: &Response) -> Option<String> {
    response
        .headers()
        .get(header::SET_COOKIE)
        .map(|value| value.to_str().unwrap().to_owned())
}

/// The `name=value` pair of the flash cookie a page sets, ready to send back.
pub fn flash_cookie(response: &Response) -> Option<String> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter(|value| value.starts_with("cocat_flash="))
        .find_map(|value| value.split(';').next().map(str::to_owned))
}

/// Whether the response clears the flash cookie, as the page that showed it must.
pub fn flash_cleared(response: &Response) -> bool {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| value.starts_with("cocat_flash=") && value.contains("Max-Age=0"))
}

/// The session token a Set-Cookie header carries.
pub fn cookie_token(cookie: &str) -> String {
    cookie
        .split(';')
        .next()
        .and_then(|pair| pair.split_once('='))
        .map(|(_, value)| value.to_owned())
        .expect("a name=value cookie")
}

pub async fn body_text(response: Response) -> String {
    let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// The token a mailed link carries after its `#`.
///
/// The body travels quoted-printable, so a long link is folded with a soft line break (`=`
/// then a newline); unfold that before reading the link.
pub fn link_token(mail: &str, path: &str) -> String {
    let unfolded = mail.replace("=\r\n", "").replace("=\n", "");
    unfolded
        .split(path)
        .nth(1)
        .and_then(|rest| rest.split_once('#'))
        .and_then(|(_, tail)| tail.split_whitespace().next())
        .unwrap_or_else(|| panic!("no {path}#token in {mail}"))
        .to_owned()
}
