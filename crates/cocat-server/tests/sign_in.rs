//! Browser sign-in for apps: the app asks for a request, the person opens its link, signs in,
//! compares the code and allows it; the app, polling, receives a token once.

mod ui_support;

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use cocat_server::ws;
use serde_json::{Value, json};
use tower::ServiceExt;
use ui_support::{Fixture, PUBLIC_URL, body_text, form, location};

const PEER: &str = "203.0.113.7:41000";
const MEMBER: &str = "member@example.org";
const NAME: &str = "Python on lpp-jupyter";

async fn call(
    fixture: &Fixture,
    method: Method,
    path: &str,
    bearer: Option<&str>,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> Response {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(token) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let body = match body {
        Some(body) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let mut request = request.body(body).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(PEER.parse::<std::net::SocketAddr>().unwrap()));
    ws::router(fixture.state.clone())
        .oneshot(request)
        .await
        .unwrap()
}

async fn json_of(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn ask(fixture: &Fixture) -> Value {
    let response = call(
        fixture,
        Method::POST,
        "/api/sign-in-requests",
        None,
        Some(json!({ "name": NAME })),
        &[],
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    json_of(response).await
}

async fn poll(fixture: &Fixture, poll: &str) -> (StatusCode, Value) {
    let response = call(
        fixture,
        Method::POST,
        "/api/sign-in-requests/poll",
        None,
        Some(json!({ "poll": poll })),
        &[],
    )
    .await;
    (response.status(), json_of(response).await)
}

/// Polls come every two seconds in real use; the test moves the last poll back instead of
/// waiting.
async fn forget_last_poll(fixture: &Fixture) {
    sqlx::query("UPDATE sign_in_request SET polled_at = NULL")
        .execute(&fixture.state.pool)
        .await
        .unwrap();
}

async fn decide(fixture: &Fixture, browser: &str, code: &str, decision: &str) -> Response {
    fixture
        .post(
            "/ui/connect",
            Some(browser),
            &form(&[("code", code), ("decision", decision)]),
        )
        .await
}

#[tokio::test]
async fn a_request_hands_out_a_code_a_link_and_a_poll_id() {
    let fixture = Fixture::start_public().await;

    let request = ask(&fixture).await;

    let code = request["code"].as_str().unwrap();
    assert_eq!(code.len(), 9, "{code}");
    assert_eq!(&code[4..5], "-", "{code}");
    assert!(
        code.chars()
            .filter(|c| *c != '-')
            .all(|c| "0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(c)),
        "{code}"
    );
    assert_eq!(
        request["url"],
        format!("{PUBLIC_URL}/ui/connect?code={code}")
    );
    assert!(request["poll"].as_str().unwrap().len() >= 32);
    assert_eq!(request["expires_in"], 600);
    assert_eq!(request["interval"], 2);
}

#[tokio::test]
async fn without_a_public_url_the_link_uses_the_request_host() {
    let fixture = Fixture::start().await;

    let response = call(
        &fixture,
        Method::POST,
        "/api/sign-in-requests",
        None,
        Some(json!({ "name": NAME })),
        &[("host", "127.0.0.1:6570")],
    )
    .await;

    let request = json_of(response).await;
    let code = request["code"].as_str().unwrap();
    assert_eq!(
        request["url"],
        format!("http://127.0.0.1:6570/ui/connect?code={code}")
    );
}

#[tokio::test]
async fn a_request_needs_a_name() {
    let fixture = Fixture::start().await;

    let response = call(
        &fixture,
        Method::POST,
        "/api/sign-in-requests",
        None,
        Some(json!({ "name": "  " })),
        &[],
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn requests_are_rate_limited() {
    let fixture = Fixture::start().await;
    for _ in 0..10 {
        ask(&fixture).await;
    }

    let response = call(
        &fixture,
        Method::POST,
        "/api/sign-in-requests",
        None,
        Some(json!({ "name": NAME })),
        &[],
    )
    .await;

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn an_allowed_request_hands_its_token_out_once() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let browser = fixture.token(user).await;
    let request = ask(&fixture).await;
    let code = request["code"].as_str().unwrap();
    let poll_id = request["poll"].as_str().unwrap();

    assert_eq!(
        poll(&fixture, poll_id).await,
        (StatusCode::OK, json!({ "status": "pending" }))
    );

    let page = body_text(
        fixture
            .get(&format!("/ui/connect?code={code}"), Some(&browser))
            .await,
    )
    .await;
    assert!(
        page.contains(&format!(
            "<strong>{NAME}</strong> wants to use your account"
        )),
        "{page}"
    );
    assert!(page.contains(code), "{page}");
    let allowed = decide(&fixture, &browser, code, "allow").await;
    assert_eq!(allowed.status(), StatusCode::OK);
    let allowed = body_text(allowed).await;
    assert!(
        allowed.contains(&format!("{NAME} is signed in")),
        "{allowed}"
    );

    forget_last_poll(&fixture).await;
    let (status, approved) = poll(&fixture, poll_id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(approved["status"], "approved");
    let token = approved["token"].as_str().unwrap();
    let me = call(&fixture, Method::GET, "/api/me", Some(token), None, &[]).await;
    assert_eq!(me.status(), StatusCode::OK);
    let tokens =
        json_of(call(&fixture, Method::GET, "/api/tokens", Some(token), None, &[]).await).await;
    assert_eq!(tokens[0]["name"], NAME);

    forget_last_poll(&fixture).await;
    assert_eq!(
        poll(&fixture, poll_id).await,
        (StatusCode::OK, json!({ "status": "expired" })),
        "the token is handed out once, then the request is gone"
    );
}

#[tokio::test]
async fn a_denied_request_says_so_and_ends() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let browser = fixture.token(user).await;
    let request = ask(&fixture).await;
    let code = request["code"].as_str().unwrap();
    let poll_id = request["poll"].as_str().unwrap();

    let denied = body_text(decide(&fixture, &browser, code, "deny").await).await;

    assert!(denied.contains("was not allowed"), "{denied}");
    assert_eq!(
        poll(&fixture, poll_id).await,
        (StatusCode::OK, json!({ "status": "denied" }))
    );
    forget_last_poll(&fixture).await;
    assert_eq!(
        poll(&fixture, poll_id).await.1,
        json!({ "status": "expired" })
    );
}

#[tokio::test]
async fn an_expired_request_cannot_be_allowed_or_polled() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let browser = fixture.token(user).await;
    let request = ask(&fixture).await;
    let code = request["code"].as_str().unwrap();
    let poll_id = request["poll"].as_str().unwrap();
    sqlx::query("UPDATE sign_in_request SET expires_at = '2000-01-01T00:00:00.000Z'")
        .execute(&fixture.state.pool)
        .await
        .unwrap();

    let page = body_text(
        fixture
            .get(&format!("/ui/connect?code={code}"), Some(&browser))
            .await,
    )
    .await;
    assert!(page.contains("This sign-in link expired"), "{page}");
    let late = body_text(decide(&fixture, &browser, code, "allow").await).await;
    assert!(late.contains("This sign-in link expired"), "{late}");
    assert_eq!(
        poll(&fixture, poll_id).await,
        (StatusCode::OK, json!({ "status": "expired" }))
    );
}

#[tokio::test]
async fn an_unknown_poll_id_reads_as_expired() {
    let fixture = Fixture::start().await;

    assert_eq!(
        poll(&fixture, "not-a-poll-id").await,
        (StatusCode::OK, json!({ "status": "expired" }))
    );
}

#[tokio::test]
async fn polling_too_fast_is_told_to_slow_down() {
    let fixture = Fixture::start().await;
    let request = ask(&fixture).await;
    let poll_id = request["poll"].as_str().unwrap();
    assert_eq!(poll(&fixture, poll_id).await.0, StatusCode::OK);

    let (status, body) = poll(&fixture, poll_id).await;

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body, json!({ "detail": "slow_down" }));
}

#[tokio::test]
async fn the_connect_page_sends_a_stranger_to_sign_in_and_back() {
    let fixture = Fixture::start().await;
    let request = ask(&fixture).await;
    let code = request["code"].as_str().unwrap();

    let response = fixture.get(&format!("/ui/connect?code={code}"), None).await;

    let target = location(&response);
    assert_eq!(
        target,
        format!("/ui/login?next=%2Fui%2Fconnect%3Fcode%3D{code}")
    );
    fixture.verified(MEMBER).await;
    let signed_in = fixture
        .login(
            MEMBER,
            ui_support::PASSWORD,
            Some(&format!("/ui/connect?code={code}")),
        )
        .await;
    assert_eq!(location(&signed_in), format!("/ui/connect?code={code}"));
}

#[tokio::test]
async fn a_code_is_case_and_space_tolerant_on_the_page() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let browser = fixture.token(user).await;
    let request = ask(&fixture).await;
    let code = request["code"].as_str().unwrap().to_lowercase();

    let page = body_text(
        fixture
            .get(&format!("/ui/connect?code=%20{code}"), Some(&browser))
            .await,
    )
    .await;

    assert!(page.contains("wants to use your account"), "{page}");
}

#[tokio::test]
async fn the_connect_page_explains_itself() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let browser = fixture.token(user).await;
    let request = ask(&fixture).await;
    let code = request["code"].as_str().unwrap();

    let page = body_text(
        fixture
            .get(&format!("/ui/connect?code={code}"), Some(&browser))
            .await,
    )
    .await;

    assert!(page.contains("#apps"), "the hint links to the help: {page}");
    assert!(page.contains(">Allow<"), "{page}");
    assert!(page.contains(">Deny<"), "{page}");
}
