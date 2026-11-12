//! A page on another site must not be able to start a session, or write anything, through
//! the browser. The guard covers every state-changing method on every path, not just the UI
//! forms; a client that sends neither `Origin` nor `Sec-Fetch-Site` still passes.

mod ui_support;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use cocat_server::auth::COOKIE_NAME;
use cocat_server::{db, ws};
use tower::ServiceExt;
use ui_support::{Fixture, PASSWORD};

const PEER: &str = "203.0.113.7:41000";
const ADMIN: &str = "admin@example.org";

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

fn login_request(headers: &[(&str, &str)]) -> Request<Body> {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/auth/jwt/login")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    request
        .body(Body::from(format!("username={ADMIN}&password={PASSWORD}")))
        .unwrap()
}

fn admin_request(
    method: Method,
    path: &str,
    token: &str,
    headers: &[(&str, &str)],
) -> Request<Body> {
    write_request(method, path, token, headers, None)
}

fn write_request(
    method: Method,
    path: &str,
    token: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::COOKIE, format!("{COOKIE_NAME}={token}"));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    if body.is_some() {
        request = request.header(header::CONTENT_TYPE, "application/json");
    }
    request
        .body(body.map_or_else(Body::empty, |body| Body::from(body.to_owned())))
        .unwrap()
}

async fn admin_login(fixture: &Fixture, headers: &[(&str, &str)]) -> Response {
    send(fixture, login_request(headers)).await
}

fn cookie(response: &Response) -> String {
    response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_owned())
        .unwrap_or_default()
}

#[tokio::test]
async fn cross_site_login_form_refused() {
    let fixture = Fixture::start().await;
    db::create_user(&fixture.state.pool, ADMIN, PASSWORD, true, true)
        .await
        .unwrap();

    let foreign = admin_login(
        &fixture,
        &[("Host", "example.org"), ("Origin", "https://evil.example")],
    )
    .await;
    assert_eq!(foreign.status(), StatusCode::FORBIDDEN);
    assert!(cookie(&foreign).is_empty(), "a refused login set a cookie");

    let cross = admin_login(&fixture, &[("Sec-Fetch-Site", "cross-site")]).await;
    assert_eq!(cross.status(), StatusCode::FORBIDDEN);
    assert!(cookie(&cross).is_empty(), "a refused login set a cookie");
}

#[tokio::test]
async fn cross_site_api_write_refused() {
    let fixture = Fixture::start().await;
    db::create_user(&fixture.state.pool, ADMIN, PASSWORD, true, true)
        .await
        .unwrap();
    let member = fixture.verified("member@example.org").await;

    let login = admin_login(&fixture, &[]).await;
    assert_eq!(login.status(), StatusCode::NO_CONTENT);
    let token = cookie_token(&login);
    let token = token.as_str();

    let patched = send(
        &fixture,
        write_request(
            Method::PATCH,
            "/users/me",
            token,
            &[("Sec-Fetch-Site", "cross-site")],
            Some("{}"),
        ),
    )
    .await;
    assert_eq!(patched.status(), StatusCode::FORBIDDEN);

    let deleted = send(
        &fixture,
        admin_request(
            Method::DELETE,
            &format!("/users/{member}"),
            token,
            &[("Origin", "https://evil.example"), ("Host", "example.org")],
        ),
    )
    .await;
    assert_eq!(deleted.status(), StatusCode::FORBIDDEN);

    let same_origin_delete = send(
        &fixture,
        admin_request(
            Method::DELETE,
            &format!("/users/{member}"),
            token,
            &[("Origin", "http://example.org"), ("Host", "example.org")],
        ),
    )
    .await;
    assert_eq!(same_origin_delete.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn client_without_browser_headers_still_logs_in() {
    let fixture = Fixture::start().await;
    db::create_user(&fixture.state.pool, ADMIN, PASSWORD, true, true)
        .await
        .unwrap();

    let login = admin_login(&fixture, &[]).await;
    assert_eq!(login.status(), StatusCode::NO_CONTENT);
    assert!(
        !cookie(&login).is_empty(),
        "a headerless login got no cookie"
    );

    let read = send(
        &fixture,
        admin_request(Method::GET, "/api/me", &cookie_token(&login), &[]),
    )
    .await;
    assert_eq!(read.status(), StatusCode::OK);
}

fn cookie_token(response: &Response) -> String {
    cookie(response)
        .split('=')
        .nth(1)
        .expect("a cookie value")
        .split(';')
        .next()
        .expect("a cookie value")
        .to_owned()
}

fn socket_request(headers: &[(&str, &str)]) -> Request<Body> {
    let mut request = Request::builder()
        .method(Method::GET)
        .uri("/room/lab")
        .header(header::CONNECTION, "upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_VERSION, "13")
        .header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    request.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn another_site_cannot_open_a_project_socket_with_the_visitors_cookie() {
    let fixture = Fixture::start().await;

    let foreign = &[
        ("Origin", "https://evil.example.org"),
        ("Host", "example.org"),
    ];
    let refused = send(&fixture, socket_request(foreign)).await;
    let cross = send(
        &fixture,
        socket_request(&[("Sec-Fetch-Site", "cross-site")]),
    )
    .await;
    let same = &[("Origin", "https://example.org"), ("Host", "example.org")];
    let allowed = send(&fixture, socket_request(same)).await;
    let python = send(&fixture, socket_request(&[])).await;

    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    assert_eq!(cross.status(), StatusCode::FORBIDDEN);
    assert_ne!(allowed.status(), StatusCode::FORBIDDEN);
    assert_ne!(
        python.status(),
        StatusCode::FORBIDDEN,
        "no Origin: a script, not a page"
    );
}

#[tokio::test]
async fn private_answers_are_not_cached_by_others_nor_framed_by_another_site() {
    let fixture = Fixture::start().await;
    db::create_user(&fixture.state.pool, ADMIN, PASSWORD, true, true)
        .await
        .unwrap();
    let token = cookie_token(&admin_login(&fixture, &[]).await);

    let me = send(&fixture, admin_request(Method::GET, "/api/me", &token, &[])).await;
    let page = send(
        &fixture,
        admin_request(Method::GET, "/ui/account", &token, &[]),
    )
    .await;

    for response in [&me, &page] {
        let header = |name| response.headers().get(name).map(|v| v.to_str().unwrap());
        assert_eq!(header(header::CACHE_CONTROL), Some("private, no-cache"));
        assert_eq!(header(header::X_FRAME_OPTIONS), Some("SAMEORIGIN"));
    }
}
