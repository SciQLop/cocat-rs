//! The server keeps every sign-in in its own table, so a cookie can be taken away: a logout
//! ends one session, a password change or a deactivation ends all of them.

mod ui_support;

use std::time::SystemTime;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use cocat_server::auth::COOKIE_NAME;
use cocat_server::{accounts, db, ws};
use tower::ServiceExt;
use ui_support::{Fixture, PASSWORD};

const PEER: &str = "203.0.113.7:41000";
const NEW_PASSWORD: &str = "violet canyon ferry drum";

async fn send(fixture: &Fixture, request: Request<Body>) -> Response {
    let mut request = request;
    request
        .extensions_mut()
        .insert(ConnectInfo(PEER.parse::<std::net::SocketAddr>().unwrap()));
    ws::router(fixture.state.clone())
        .oneshot(request)
        .await
        .unwrap()
}

fn with_cookie(request: axum::http::request::Builder, token: &str) -> axum::http::request::Builder {
    request.header(header::COOKIE, format!("{COOKIE_NAME}={token}"))
}

async fn api_login(fixture: &Fixture, email: &str, password: &str) -> String {
    let form = format!("username={email}&password={password}");
    let response = send(
        fixture,
        Request::builder()
            .method(Method::POST)
            .uri("/auth/jwt/login")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    cookie_token(&response)
}

async fn api_logout(fixture: &Fixture, token: &str) -> Response {
    let request = with_cookie(
        Request::builder()
            .method(Method::POST)
            .uri("/auth/jwt/logout"),
        token,
    );
    send(fixture, request.body(Body::empty()).unwrap()).await
}

async fn me_status(fixture: &Fixture, token: &str) -> StatusCode {
    let request = with_cookie(Request::builder().method(Method::GET).uri("/api/me"), token);
    send(fixture, request.body(Body::empty()).unwrap())
        .await
        .status()
}

async fn patch_me(fixture: &Fixture, token: &str, body: &str) -> Response {
    let request = with_cookie(
        Request::builder()
            .method(Method::PATCH)
            .uri("/users/me")
            .header(header::CONTENT_TYPE, "application/json"),
        token,
    );
    send(fixture, request.body(Body::from(body.to_owned())).unwrap()).await
}

async fn session_rows(fixture: &Fixture) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM session")
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap()
}

async fn session_hashes(fixture: &Fixture) -> Vec<String> {
    sqlx::query_scalar("SELECT token_hash FROM session")
        .fetch_all(&fixture.state.pool)
        .await
        .unwrap()
}

fn set_cookie(response: &Response) -> String {
    response
        .headers()
        .get(header::SET_COOKIE)
        .expect("a session cookie")
        .to_str()
        .unwrap()
        .to_owned()
}

fn cookie_token(response: &Response) -> String {
    let cookie = set_cookie(response);
    let (name, value) = cookie.split_once('=').expect("a cookie pair");
    assert_eq!(name, COOKIE_NAME);
    value.split(';').next().unwrap().to_owned()
}

#[tokio::test]
async fn two_logins_are_two_sessions() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;

    let first = api_login(&fixture, "member@example.org", PASSWORD).await;
    let second = api_login(&fixture, "member@example.org", PASSWORD).await;

    assert_ne!(first, second);
    assert_eq!(session_rows(&fixture).await, 2);
    assert_eq!(me_status(&fixture, &first).await, StatusCode::OK);
    assert_eq!(me_status(&fixture, &second).await, StatusCode::OK);
}

#[tokio::test]
async fn logout_ends_the_session() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;

    let api = api_login(&fixture, "member@example.org", PASSWORD).await;
    let other = api_login(&fixture, "member@example.org", PASSWORD).await;
    let response = api_logout(&fixture, &api).await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(me_status(&fixture, &api).await, StatusCode::UNAUTHORIZED);
    assert_eq!(me_status(&fixture, &other).await, StatusCode::OK);

    let request = with_cookie(
        Request::builder().method(Method::POST).uri("/ui/logout"),
        &other,
    );
    let response = send(&fixture, request.body(Body::empty()).unwrap()).await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(me_status(&fixture, &other).await, StatusCode::UNAUTHORIZED);
    assert_eq!(session_rows(&fixture).await, 0);
}

#[tokio::test]
async fn password_change_ends_every_session() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;

    let first = api_login(&fixture, "member@example.org", PASSWORD).await;
    let second = api_login(&fixture, "member@example.org", PASSWORD).await;
    let response = patch_me(
        &fixture,
        &first,
        &format!(r#"{{"password":"{NEW_PASSWORD}","current_password":"{PASSWORD}"}}"#),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(me_status(&fixture, &first).await, StatusCode::UNAUTHORIZED);
    assert_eq!(me_status(&fixture, &second).await, StatusCode::UNAUTHORIZED);
    assert_eq!(session_rows(&fixture).await, 0);
    assert!(
        db::user_by_email(&fixture.state.pool, "member@example.org")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn password_reset_ends_every_session() {
    let fixture = Fixture::start().await;
    let id = fixture.verified("member@example.org").await;
    let first = api_login(&fixture, "member@example.org", PASSWORD).await;
    let second = api_login(&fixture, "member@example.org", PASSWORD).await;
    let user = db::user_by_id(&fixture.state.pool, id)
        .await
        .unwrap()
        .unwrap();
    let token = accounts::reset_token(
        user.id,
        &user.hashed_password,
        &fixture.state.secret,
        SystemTime::now(),
    )
    .unwrap();

    accounts::reset_password(&fixture.state, &token, NEW_PASSWORD)
        .await
        .unwrap();

    assert_eq!(me_status(&fixture, &first).await, StatusCode::UNAUTHORIZED);
    assert_eq!(me_status(&fixture, &second).await, StatusCode::UNAUTHORIZED);
    assert_eq!(session_rows(&fixture).await, 0);
}

#[tokio::test]
async fn deactivation_ends_every_session() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;
    let first = api_login(&fixture, "member@example.org", PASSWORD).await;
    let second = api_login(&fixture, "member@example.org", PASSWORD).await;

    fixture.deactivate("member@example.org").await;

    assert_eq!(me_status(&fixture, &first).await, StatusCode::UNAUTHORIZED);
    assert_eq!(me_status(&fixture, &second).await, StatusCode::UNAUTHORIZED);
    assert_eq!(session_rows(&fixture).await, 0);
}

#[tokio::test]
async fn deleting_the_user_drops_sessions() {
    let fixture = Fixture::start().await;
    let id = fixture.verified("member@example.org").await;
    let token = api_login(&fixture, "member@example.org", PASSWORD).await;

    assert!(accounts::delete(&fixture.state, id).await.unwrap());

    assert_eq!(session_rows(&fixture).await, 0);
    assert_eq!(me_status(&fixture, &token).await, StatusCode::UNAUTHORIZED);
}

async fn seconds_left(fixture: &Fixture) -> i64 {
    sqlx::query_scalar(
        "SELECT CAST((julianday(expires_at) - julianday('now')) * 86400 AS INTEGER) FROM session",
    )
    .fetch_one(&fixture.state.pool)
    .await
    .unwrap()
}

async fn age_session(fixture: &Fixture, created: &str, last_used: &str, expires: &str) {
    sqlx::query(
        "UPDATE session SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?),
                            last_used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?),
                            expires_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?)",
    )
    .bind(created)
    .bind(last_used)
    .bind(expires)
    .execute(&fixture.state.pool)
    .await
    .unwrap();
}

const DAY: i64 = 86_400;

#[tokio::test]
async fn a_new_browser_session_lasts_thirty_days_unused() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;
    api_login(&fixture, "member@example.org", PASSWORD).await;

    let left = seconds_left(&fixture).await;
    assert!((29 * DAY..=30 * DAY).contains(&left), "{left}");
}

#[tokio::test]
async fn a_browser_session_in_use_slides_thirty_days_on() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;
    let token = api_login(&fixture, "member@example.org", PASSWORD).await;
    age_session(&fixture, "-5 days", "-2 hours", "+1 minute").await;

    assert_eq!(me_status(&fixture, &token).await, StatusCode::OK);

    let left = seconds_left(&fixture).await;
    assert!((29 * DAY..=30 * DAY).contains(&left), "{left}");
}

#[tokio::test]
async fn a_browser_session_used_this_hour_writes_nothing() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;
    let token = api_login(&fixture, "member@example.org", PASSWORD).await;
    age_session(&fixture, "-5 days", "-10 minutes", "+3 days").await;

    assert_eq!(me_status(&fixture, &token).await, StatusCode::OK);

    let left = seconds_left(&fixture).await;
    assert!(left <= 3 * DAY, "{left}");
}

#[tokio::test]
async fn a_browser_session_ends_ninety_days_after_sign_in_however_used() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;
    let token = api_login(&fixture, "member@example.org", PASSWORD).await;
    age_session(&fixture, "-89 days", "-2 hours", "+1 minute").await;

    assert_eq!(me_status(&fixture, &token).await, StatusCode::OK);

    let left = seconds_left(&fixture).await;
    assert!(left <= DAY, "the session cannot outlive 90 days: {left}");
}

#[tokio::test]
async fn expired_session_refused() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;
    let token = api_login(&fixture, "member@example.org", PASSWORD).await;

    sqlx::query("UPDATE session SET expires_at = '2000-01-01T00:00:00.000Z'")
        .execute(&fixture.state.pool)
        .await
        .unwrap();

    assert_eq!(me_status(&fixture, &token).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn unknown_token_refused() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;

    assert_eq!(
        me_status(&fixture, "nonsense").await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn only_the_hash_is_stored() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;
    let token = api_login(&fixture, "member@example.org", PASSWORD).await;

    let hashes = session_hashes(&fixture).await;
    assert_eq!(hashes.len(), 1);
    let hash = &hashes[0];
    assert_eq!(hash.len(), 64);
    assert!(
        hash.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    assert_ne!(hash, &token);
    let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session WHERE token_hash = ?")
        .bind(&token)
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    assert_eq!(stored, 0);
}

#[tokio::test]
async fn expired_rows_are_pruned_on_the_next_login() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;
    let token = api_login(&fixture, "member@example.org", PASSWORD).await;
    sqlx::query("UPDATE session SET expires_at = '2000-01-01T00:00:00.000Z'")
        .execute(&fixture.state.pool)
        .await
        .unwrap();

    let fresh = api_login(&fixture, "member@example.org", PASSWORD).await;

    assert_eq!(session_rows(&fixture).await, 1);
    assert_eq!(me_status(&fixture, &fresh).await, StatusCode::OK);
    assert_eq!(me_status(&fixture, &token).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_cookie_keeps_the_sign_in_for_ninety_days() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;

    let response = send(
        &fixture,
        Request::builder()
            .method(Method::POST)
            .uri("/auth/jwt/login")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(format!(
                "username=member@example.org&password={PASSWORD}"
            )))
            .unwrap(),
    )
    .await;
    let cookie = set_cookie(&response);
    assert!(cookie.contains("Path=/"), "{cookie}");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Lax"), "{cookie}");
    assert!(cookie.contains("Secure"), "{cookie}");
    assert!(cookie.contains("Max-Age=7776000"), "{cookie}");
}

#[tokio::test]
async fn logout_clears_the_cookie() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;
    let token = api_login(&fixture, "member@example.org", PASSWORD).await;

    let response = api_logout(&fixture, &token).await;
    let cookie = set_cookie(&response);

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(cookie.starts_with(&format!("{COOKIE_NAME}=;")), "{cookie}");
    assert!(cookie.contains("Max-Age=0"), "{cookie}");
}

#[tokio::test]
async fn a_tampered_token_is_refused() {
    let fixture = Fixture::start().await;
    fixture.verified("member@example.org").await;
    let token = api_login(&fixture, "member@example.org", PASSWORD).await;
    let mut tampered = token.clone();
    tampered.push('x');

    assert_eq!(
        me_status(&fixture, &tampered).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(me_status(&fixture, &token).await, StatusCode::OK);
}
