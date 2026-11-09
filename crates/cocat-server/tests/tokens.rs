//! Named sessions: the tokens apps sign in with. They last until revoked or six months
//! unused, travel as `Authorization: Bearer`, and survive a password change.

mod ui_support;

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use cocat_server::{auth, ws};
use serde_json::Value;
use tower::ServiceExt;
use ui_support::{Fixture, PASSWORD, body_text, form, location};
use uuid::Uuid;

const PEER: &str = "203.0.113.7:41000";
const MEMBER: &str = "member@example.org";
const NEW_PASSWORD: &str = "violet canyon ferry drum";

async fn call(fixture: &Fixture, method: Method, path: &str, bearer: Option<&str>) -> Response {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(token) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let mut request = request.body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(PEER.parse::<std::net::SocketAddr>().unwrap()));
    ws::router(fixture.state.clone())
        .oneshot(request)
        .await
        .unwrap()
}

async fn json(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn me_status(fixture: &Fixture, bearer: &str) -> StatusCode {
    call(fixture, Method::GET, "/api/me", Some(bearer))
        .await
        .status()
}

async fn new_token(fixture: &Fixture, user: Uuid, name: &str) -> String {
    auth::start_token(&fixture.state.pool, user, name)
        .await
        .unwrap()
}

async fn last_used(fixture: &Fixture, name: &str) -> String {
    sqlx::query_scalar("SELECT last_used_at FROM session WHERE name = ?")
        .bind(name)
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap()
}

async fn sql_time(fixture: &Fixture, modifier: &str) -> String {
    sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?)")
        .bind(modifier)
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_bearer_token_signs_api_calls_in() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = new_token(&fixture, user, "Python on laptop").await;

    let me = call(&fixture, Method::GET, "/api/me", Some(&token)).await;

    assert_eq!(me.status(), StatusCode::OK);
    assert_eq!(json(me).await["email"], MEMBER);
    assert_eq!(
        me_status(&fixture, "not-a-token").await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_browser_session_works_as_a_bearer_token_too() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let cookie_value = fixture.token(user).await;

    assert_eq!(me_status(&fixture, &cookie_value).await, StatusCode::OK);
}

#[tokio::test]
async fn the_token_list_shows_named_tokens_only() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let other = fixture.verified("other@example.org").await;
    let token = new_token(&fixture, user, "Python on laptop").await;
    new_token(&fixture, user, "SciQLop on desktop").await;
    new_token(&fixture, other, "Not mine").await;
    fixture.token(user).await;

    let listed = json(call(&fixture, Method::GET, "/api/tokens", Some(&token)).await).await;

    let names: Vec<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 2, "{listed}");
    assert!(names.contains(&"Python on laptop"), "{listed}");
    assert!(names.contains(&"SciQLop on desktop"), "{listed}");
    let row = &listed[0];
    assert!(row["id"].as_str().is_some_and(|id| !id.is_empty()), "{row}");
    assert!(row["created_at"].is_string(), "{row}");
    assert!(row["last_used_at"].is_string(), "{row}");
    assert!(
        !listed.to_string().contains(&token),
        "the list never shows a token"
    );
}

#[tokio::test]
async fn revoking_a_token_by_id_ends_it() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let keeper = new_token(&fixture, user, "Keeper").await;
    let doomed = new_token(&fixture, user, "Doomed").await;
    let listed = json(call(&fixture, Method::GET, "/api/tokens", Some(&keeper)).await).await;
    let id = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "Doomed")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let response = call(
        &fixture,
        Method::DELETE,
        &format!("/api/tokens/{id}"),
        Some(&keeper),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(me_status(&fixture, &doomed).await, StatusCode::UNAUTHORIZED);
    assert_eq!(me_status(&fixture, &keeper).await, StatusCode::OK);
    let again = call(
        &fixture,
        Method::DELETE,
        &format!("/api/tokens/{id}"),
        Some(&keeper),
    )
    .await;
    assert_eq!(again.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn nobody_revokes_someone_else_s_token() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let other = fixture.verified("other@example.org").await;
    let mine = new_token(&fixture, user, "Mine").await;
    let theirs = new_token(&fixture, other, "Theirs").await;
    let listed = json(call(&fixture, Method::GET, "/api/tokens", Some(&theirs)).await).await;
    let id = listed[0]["id"].as_str().unwrap().to_owned();

    let response = call(
        &fixture,
        Method::DELETE,
        &format!("/api/tokens/{id}"),
        Some(&mine),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(me_status(&fixture, &theirs).await, StatusCode::OK);
}

#[tokio::test]
async fn revoking_the_current_token_signs_the_app_out() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = new_token(&fixture, user, "Python on laptop").await;

    let response = call(
        &fixture,
        Method::DELETE,
        "/api/tokens/current",
        Some(&token),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(me_status(&fixture, &token).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        call(
            &fixture,
            Method::DELETE,
            "/api/tokens/current",
            Some(&token)
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_password_change_keeps_tokens_and_ends_browser_sessions() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = new_token(&fixture, user, "Python on laptop").await;
    let browser = fixture.token(user).await;

    let response = fixture
        .post(
            "/ui/account/password",
            Some(&browser),
            &form(&[
                ("current", PASSWORD),
                ("new", NEW_PASSWORD),
                ("repeat", NEW_PASSWORD),
            ]),
        )
        .await;

    assert_eq!(location(&response), "/ui/login?error=password_changed");
    assert_eq!(
        me_status(&fixture, &browser).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(me_status(&fixture, &token).await, StatusCode::OK);
}

#[tokio::test]
async fn deactivation_ends_tokens_too() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = new_token(&fixture, user, "Python on laptop").await;

    fixture.deactivate(MEMBER).await;
    let row: Option<String> = sqlx::query_scalar("SELECT name FROM session")
        .fetch_optional(&fixture.state.pool)
        .await
        .unwrap();

    assert_eq!(me_status(&fixture, &token).await, StatusCode::UNAUTHORIZED);
    assert_eq!(row, None);
}

#[tokio::test]
async fn a_token_unused_for_six_months_expires() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let fresh = new_token(&fixture, user, "Fresh").await;
    let stale = new_token(&fixture, user, "Stale").await;
    let fresh_expiry: String =
        sqlx::query_scalar("SELECT expires_at FROM session WHERE name = 'Fresh'")
            .fetch_one(&fixture.state.pool)
            .await
            .unwrap();
    assert!(
        fresh_expiry > sql_time(&fixture, "+179 days").await,
        "{fresh_expiry}"
    );
    assert!(
        fresh_expiry < sql_time(&fixture, "+185 days").await,
        "{fresh_expiry}"
    );

    sqlx::query(
        "UPDATE session SET last_used_at = '2000-01-01T00:00:00.000Z',
                            expires_at = '2000-07-01T00:00:00.000Z'
         WHERE name = 'Stale'",
    )
    .execute(&fixture.state.pool)
    .await
    .unwrap();

    assert_eq!(me_status(&fixture, &stale).await, StatusCode::UNAUTHORIZED);
    assert_eq!(me_status(&fixture, &fresh).await, StatusCode::OK);
}

#[tokio::test]
async fn use_slides_the_expiry_at_most_once_an_hour() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = new_token(&fixture, user, "Python on laptop").await;
    let created = last_used(&fixture, "Python on laptop").await;

    assert_eq!(me_status(&fixture, &token).await, StatusCode::OK);
    assert_eq!(
        last_used(&fixture, "Python on laptop").await,
        created,
        "a use within the hour writes nothing"
    );

    let two_hours_ago = sql_time(&fixture, "-2 hours").await;
    sqlx::query("UPDATE session SET last_used_at = ?, expires_at = ? WHERE name = ?")
        .bind(&two_hours_ago)
        .bind(sql_time(&fixture, "+1 minute").await)
        .bind("Python on laptop")
        .execute(&fixture.state.pool)
        .await
        .unwrap();

    assert_eq!(me_status(&fixture, &token).await, StatusCode::OK);
    let touched = last_used(&fixture, "Python on laptop").await;
    assert!(touched > two_hours_ago, "{touched} <= {two_hours_ago}");
    let expires: String =
        sqlx::query_scalar("SELECT expires_at FROM session WHERE name = 'Python on laptop'")
            .fetch_one(&fixture.state.pool)
            .await
            .unwrap();
    assert!(
        expires > sql_time(&fixture, "+179 days").await,
        "the expiry slid: {expires}"
    );
}

#[tokio::test]
async fn the_account_page_lists_and_revokes_signed_in_apps() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = new_token(&fixture, user, "Python on lpp-jupyter").await;
    let browser = fixture.token(user).await;

    let page = body_text(fixture.get("/ui/account", Some(&browser)).await).await;

    assert!(page.contains("Signed-in apps"), "{page}");
    assert!(page.contains("Python on lpp-jupyter"), "{page}");
    let id: String = sqlx::query_scalar("SELECT id FROM session WHERE name IS NOT NULL")
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    let action = format!("/ui/account/tokens/{id}/revoke");
    assert!(page.contains(&action), "{page}");

    let response = fixture.post(&action, Some(&browser), "").await;

    let (path, landed) = fixture.follow(&response, &browser).await;
    assert_eq!(path, "/ui/account");
    assert!(
        landed.contains("Python on lpp-jupyter is signed out"),
        "{landed}"
    );
    assert_eq!(me_status(&fixture, &token).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_account_page_says_when_no_app_is_signed_in() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let browser = fixture.token(user).await;

    let page = body_text(fixture.get("/ui/account", Some(&browser)).await).await;

    assert!(page.contains("No app is signed in"), "{page}");
}
