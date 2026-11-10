use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use axum::Form;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use cocat_server::auth::{self, AuthError, LoginForm};
use cocat_server::room::RoomManager;
use cocat_server::{AppState, db, links::Links, ratelimit::LoginLimit};
use cookie::{Cookie, SameSite};
use sqlx::SqlitePool;
use tempfile::TempDir;

async fn seeded_db() -> (TempDir, SqlitePool) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let pool = db::open(&db_path).await.unwrap();
    (dir, pool)
}

fn secret_path(db_path: &Path) -> PathBuf {
    let mut path = db_path.as_os_str().to_owned();
    path.push(".secret");
    PathBuf::from(path)
}

fn form(email: &str, password: &str) -> Form<LoginForm> {
    Form(LoginForm {
        username: email.to_owned(),
        password: password.to_owned(),
    })
}

#[test]
fn the_cookie_is_read_from_the_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        "theme=dark; fastapiusersauth=abc.def; other=1"
            .parse()
            .unwrap(),
    );

    assert_eq!(auth::session_cookie(&headers), Some("abc.def".to_owned()));
    assert_eq!(auth::session_cookie(&HeaderMap::new()), None);
}

#[tokio::test]
async fn verify_password_checks_the_hash() {
    let hashed = db::hash_password("hunter2").await.unwrap();

    assert!(auth::verify_password("hunter2", &hashed).await.is_ok());
    assert!(matches!(
        auth::verify_password("wrong", &hashed).await,
        Err(AuthError::BadCredentials)
    ));
}

/// A current-thread runtime makes the difference observable without timing: a hash taken on
/// the worker would return `Ready` from the first poll, and only the blocking pool can leave
/// the future pending while another task would have run.
#[test]
fn hashing_leaves_the_worker_free() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut hashing = std::pin::pin!(db::hash_password("hunter2"));
        let mut context = Context::from_waker(Waker::noop());
        assert!(
            matches!(hashing.as_mut().poll(&mut context), Poll::Pending),
            "the password hash ran on the async worker instead of the blocking pool"
        );
        let hashed = hashing.await.unwrap();

        let mut checking = std::pin::pin!(auth::verify_password("hunter2", &hashed));
        assert!(
            matches!(checking.as_mut().poll(&mut context), Poll::Pending),
            "the password check ran on the async worker instead of the blocking pool"
        );
        checking.await.unwrap();
    });
}

#[test]
fn dummy_hash_uses_the_default_cost() {
    use argon2::PasswordHasher;
    use argon2::password_hash::phc::PasswordHash;

    let dummy = auth::dummy_hash().expect("a dummy hash to compare against");
    let parsed = PasswordHash::new(dummy).unwrap();
    let fresh = argon2::Argon2::default()
        .hash_password(b"anything")
        .unwrap();

    assert_eq!(parsed.algorithm, fresh.algorithm);
    assert_eq!(
        parsed.params.to_string(),
        fresh.params.to_string(),
        "an unknown address costs a different hash than a wrong password"
    );
}

#[test]
fn the_environment_secret_wins_over_the_file() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    fs::write(secret_path(&db_path), "file-secret").unwrap();

    let secret = auth::resolve_secret(Some("environment-secret".to_owned()), &db_path).unwrap();

    assert_eq!(secret, "environment-secret");
}

#[test]
fn the_secret_is_read_next_to_the_database() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    fs::write(secret_path(&db_path), "file-secret\n").unwrap();

    assert_eq!(auth::resolve_secret(None, &db_path).unwrap(), "file-secret");
}

#[test]
fn an_empty_environment_secret_falls_back_to_the_file() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    fs::write(secret_path(&db_path), "file-secret").unwrap();

    let secret = auth::resolve_secret(Some(String::new()), &db_path).unwrap();

    assert_eq!(secret, "file-secret");
}

#[test]
fn an_empty_secret_file_is_rejected() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    fs::write(secret_path(&db_path), "  \n").unwrap();

    match auth::resolve_secret(None, &db_path).unwrap_err() {
        AuthError::Secret { source, .. } => {
            assert!(
                source.to_string().contains("secret file is empty"),
                "{source}"
            )
        }
        other => panic!("expected a secret error, got {other:?}"),
    }
}

#[test]
fn a_missing_secret_file_is_created_once_and_reused() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let path = secret_path(&db_path);

    let created = auth::resolve_secret(None, &db_path).unwrap();

    assert!(!created.is_empty());
    assert_eq!(fs::read_to_string(&path).unwrap().trim(), created);
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(auth::resolve_secret(None, &db_path).unwrap(), created);
}

#[tokio::test]
async fn login_sets_a_cookie_for_the_right_password() {
    let (dir, pool) = seeded_db().await;
    let id = db::create_user(&pool, "user@example.org", "hunter2", false, true)
        .await
        .unwrap();
    let state = AppState {
        rooms: RoomManager::new(dir.path().to_owned(), None),
        pool,
        secret: "secret".to_owned(),
        before_attach: None,
        login_limit: LoginLimit::default(),
        mailer: None,
        links: Links::default(),
        oidc: Vec::new(),
        web_dir: None,
        providers: Default::default(),
        http: Default::default(),
    };

    let response =
        auth::login_handler(State(state.clone()), form("user@example.org", "hunter2")).await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let written = response.headers()[header::SET_COOKIE].to_str().unwrap();
    let cookie = Cookie::parse(written.to_owned()).unwrap();
    assert_eq!(cookie.name(), auth::COOKIE_NAME);
    assert_eq!(cookie.path(), Some("/"));
    assert_eq!(cookie.secure(), Some(true));
    assert_eq!(cookie.http_only(), Some(true));
    assert_eq!(cookie.same_site(), Some(SameSite::Lax));

    let session = format!("{}={}", cookie.name(), cookie.value());
    let mut headers = HeaderMap::new();
    headers.insert(header::COOKIE, session.parse().unwrap());
    assert_eq!(auth::authenticate(&state.pool, &headers).await.unwrap(), id);
}

#[tokio::test]
async fn login_rejects_a_wrong_password_and_an_inactive_user() {
    let (dir, pool) = seeded_db().await;
    db::create_user(&pool, "user@example.org", "hunter2", false, true)
        .await
        .unwrap();
    let state = AppState {
        rooms: RoomManager::new(dir.path().to_owned(), None),
        pool: pool.clone(),
        secret: "secret".to_owned(),
        before_attach: None,
        login_limit: LoginLimit::default(),
        mailer: None,
        links: Links::default(),
        oidc: Vec::new(),
        web_dir: None,
        providers: Default::default(),
        http: Default::default(),
    };

    let unknown = auth::login(&pool, "nobody@example.org", "hunter2", false).await;
    assert!(matches!(unknown.unwrap_err(), AuthError::BadCredentials));

    let wrong = auth::login_handler(State(state.clone()), form("user@example.org", "wrong")).await;
    assert_eq!(wrong.status(), StatusCode::BAD_REQUEST);

    let token = auth::login(&pool, "user@example.org", "hunter2", false)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "user" SET is_active = FALSE WHERE email = ?"#)
        .bind("user@example.org")
        .execute(&pool)
        .await
        .unwrap();

    let inactive =
        auth::login_handler(State(state.clone()), form("user@example.org", "hunter2")).await;
    assert_eq!(inactive.status(), StatusCode::BAD_REQUEST);

    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        format!("fastapiusersauth={token}").parse().unwrap(),
    );
    assert!(matches!(
        auth::authenticate(&pool, &headers).await.unwrap_err(),
        AuthError::InactiveUser
    ));
}

const BCRYPT: &str = "$2b$10$0aQWymGJbVF/FHfPIDH/UeRJ9lquQk7rS2dC5egTYDuJwtoSu/sgC";

#[tokio::test]
async fn bcrypt_hash_verifies() {
    auth::verify_password("hunter2", BCRYPT)
        .await
        .expect("an older hash still verifies");
}

#[tokio::test]
async fn wrong_password_on_bcrypt_refused() {
    assert!(matches!(
        auth::verify_password("wrong", BCRYPT).await,
        Err(AuthError::BadCredentials)
    ));
}

#[tokio::test]
async fn unknown_hash_format_refused() {
    assert!(matches!(
        auth::verify_password("hunter2", "not-a-hash").await,
        Err(AuthError::BadCredentials)
    ));
}

#[tokio::test]
async fn bcrypt_login_rehashes_to_argon2() {
    let (dir, pool) = seeded_db().await;
    db::create_user(&pool, "user@example.org", "hunter2", false, true)
        .await
        .unwrap();
    let mut user = db::user_by_email(&pool, "user@example.org")
        .await
        .unwrap()
        .expect("the account exists");
    user.hashed_password = BCRYPT.to_owned();
    db::save_user(&pool, &user).await.unwrap();

    let state = AppState {
        rooms: RoomManager::new(dir.path().to_owned(), None),
        pool,
        secret: "secret".to_owned(),
        before_attach: None,
        login_limit: LoginLimit::default(),
        mailer: None,
        links: Links::default(),
        oidc: Vec::new(),
        web_dir: None,
        providers: Default::default(),
        http: Default::default(),
    };
    let response =
        auth::login_handler(State(state.clone()), form("user@example.org", "hunter2")).await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let stored = db::user_by_email(&state.pool, "user@example.org")
        .await
        .unwrap()
        .expect("the account exists");
    assert!(
        stored.hashed_password.starts_with("$argon2"),
        "the hash moved to argon2: {}",
        stored.hashed_password
    );
}
