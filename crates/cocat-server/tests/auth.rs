use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Form;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use cocat_server::auth::{self, AuthError, LoginForm};
use cocat_server::room::RoomManager;
use cocat_server::{AppState, db, links::Links, ratelimit::LoginLimit};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::Serialize;
use sqlx::SqlitePool;
use tempfile::TempDir;
use uuid::Uuid;

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

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[derive(Serialize)]
struct ForeignClaims {
    sub: String,
    aud: Vec<String>,
    exp: i64,
}

#[test]
fn a_minted_token_verifies_to_the_user_id() {
    let id = Uuid::new_v4();

    let token = auth::mint_token(id, "secret", SystemTime::now()).unwrap();

    assert_eq!(auth::verify_token(&token, "secret").unwrap(), id);
}

#[test]
fn a_token_for_another_audience_is_rejected() {
    let claims = ForeignClaims {
        sub: Uuid::new_v4().to_string(),
        aud: vec!["other".to_owned()],
        exp: now_seconds() + 3600,
    };
    let token = jsonwebtoken::encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(b"secret"),
    )
    .unwrap();

    assert!(matches!(
        auth::verify_token(&token, "secret").unwrap_err(),
        AuthError::BadToken
    ));
}

#[test]
fn an_expired_token_is_rejected() {
    let now = SystemTime::now() - Duration::from_secs(2 * 3600);

    let token = auth::mint_token(Uuid::new_v4(), "secret", now).unwrap();

    assert!(matches!(
        auth::verify_token(&token, "secret").unwrap_err(),
        AuthError::BadToken
    ));
}

#[test]
fn a_token_signed_with_another_secret_is_rejected() {
    let token = auth::mint_token(Uuid::new_v4(), "other-secret", SystemTime::now()).unwrap();

    assert!(matches!(
        auth::verify_token(&token, "secret").unwrap_err(),
        AuthError::BadToken
    ));
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
    let id = db::create_user(&pool, "user@example.org", "hunter2", false)
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
    };

    let response =
        auth::login_handler(State(state.clone()), form("user@example.org", "hunter2")).await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(cookie.starts_with("fastapiusersauth="), "{cookie}");
    assert!(
        cookie.ends_with("; Path=/; Secure; HttpOnly; SameSite=lax"),
        "{cookie}"
    );

    let token = cookie.split(';').next().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(header::COOKIE, token.parse().unwrap());
    assert_eq!(
        auth::authenticate(&state.pool, &headers, &state.secret)
            .await
            .unwrap(),
        id
    );
}

#[tokio::test]
async fn login_rejects_a_wrong_password_and_an_inactive_user() {
    let (dir, pool) = seeded_db().await;
    db::create_user(&pool, "user@example.org", "hunter2", false)
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
    };

    let unknown = auth::login(&pool, &state.secret, "nobody@example.org", "hunter2").await;
    assert!(matches!(unknown.unwrap_err(), AuthError::BadCredentials));

    let wrong = auth::login_handler(State(state.clone()), form("user@example.org", "wrong")).await;
    assert_eq!(wrong.status(), StatusCode::BAD_REQUEST);

    let token = auth::login(&pool, &state.secret, "user@example.org", "hunter2")
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
        auth::authenticate(&pool, &headers, &state.secret)
            .await
            .unwrap_err(),
        AuthError::InactiveUser
    ));
}
