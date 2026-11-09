mod support;

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use cocat_client::{
    LoginError, Room, SignInStatus, Status, poll_sign_in, rooms, sign_out, start_sign_in,
};
use support::*;
use tokio::net::TcpListener;

const ROOM: &str = "aaaaaaaa-1111-4111-8111-111111111111";

/// A server that answers `/api/rooms` and the websocket only to `Bearer secret`, and keeps the
/// handshake's `Authorization` header.
async fn bearer_only() -> (String, Arc<Mutex<Option<String>>>) {
    let seen = Arc::new(Mutex::new(None));
    let kept = Arc::clone(&seen);
    let bearer = |headers: &HeaderMap| {
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    let app = Router::new()
        .route(
            "/api/rooms",
            get(move |headers: HeaderMap| async move {
                match bearer(&headers).as_deref() {
                    Some("Bearer secret") => (StatusCode::OK, "[]"),
                    _ => (StatusCode::UNAUTHORIZED, r#"{"detail": "Unauthorized"}"#),
                }
            }),
        )
        .route(
            "/room/{room_id}",
            get(move |headers: HeaderMap| async move {
                *kept.lock().unwrap() = bearer(&headers);
                StatusCode::FORBIDDEN
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen)
}

#[tokio::test]
async fn calls_carry_the_token_as_a_bearer() {
    let (url, _) = bearer_only().await;

    assert_eq!(rooms(&url, "secret").await.unwrap(), []);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_handshake_carries_the_token_as_a_bearer() {
    let (url, seen) = bearer_only().await;
    let dir = cache();
    let room = Room::open(dir.path(), ROOM).unwrap();

    room.connect(&url, "secret").unwrap();
    room.wait_for(Status::Unauthorized, WAIT).unwrap();

    assert_eq!(seen.lock().unwrap().as_deref(), Some("Bearer secret"));
}

#[tokio::test]
async fn an_allowed_request_hands_out_a_token_that_opens_the_room() {
    let server = Server::start().await;
    let owner = user(&server, "owner@x.org").await;
    room(&server, ROOM, owner).await;
    let request = start_sign_in(&server.url(), "Python on lab").await.unwrap();
    assert_eq!(
        poll_sign_in(&server.url(), &request.poll).await.unwrap(),
        SignInStatus::Pending
    );

    cocat_server::sign_in::decide(&server.pool, &request.code, owner, true)
        .await
        .unwrap();
    let token = approved(&server, &request.poll).await;

    let dir = cache();
    let room = Room::open(dir.path(), ROOM).unwrap();
    room.connect(&server.url(), &token).unwrap();
    room.wait_synced(WAIT).unwrap();
    assert!(
        request
            .url
            .ends_with(&format!("/ui/connect?code={}", request.code))
    );
    assert_eq!(request.interval, 2);
}

#[tokio::test]
async fn polling_too_soon_still_reads_as_pending() {
    let server = Server::start().await;
    let request = start_sign_in(&server.url(), "Python on lab").await.unwrap();

    let first = poll_sign_in(&server.url(), &request.poll).await.unwrap();
    let second = poll_sign_in(&server.url(), &request.poll).await.unwrap();

    assert_eq!(
        (first, second),
        (SignInStatus::Pending, SignInStatus::Pending)
    );
}

#[tokio::test]
async fn a_denied_request_says_so() {
    let server = Server::start().await;
    let owner = user(&server, "owner@x.org").await;
    let request = start_sign_in(&server.url(), "Python on lab").await.unwrap();

    cocat_server::sign_in::decide(&server.pool, &request.code, owner, false)
        .await
        .unwrap();

    assert_eq!(
        poll_sign_in(&server.url(), &request.poll).await.unwrap(),
        SignInStatus::Denied
    );
}

#[tokio::test]
async fn an_unknown_request_is_expired() {
    let server = Server::start().await;

    assert_eq!(
        poll_sign_in(&server.url(), "no-such-request")
            .await
            .unwrap(),
        SignInStatus::Expired
    );
}

#[tokio::test]
async fn signing_out_ends_the_token() {
    let server = Server::start().await;
    let owner = user(&server, "owner@x.org").await;
    let request = start_sign_in(&server.url(), "Python on lab").await.unwrap();
    cocat_server::sign_in::decide(&server.pool, &request.code, owner, true)
        .await
        .unwrap();
    let token = approved(&server, &request.poll).await;

    sign_out(&server.url(), &token).await.unwrap();

    assert!(matches!(
        rooms(&server.url(), &token).await,
        Err(LoginError::Refused { status: 401, .. })
    ));
}

/// The token an allowed request hands out, polled past the server's one-second throttle.
async fn approved(server: &Server, poll: &str) -> String {
    sqlx::query("UPDATE sign_in_request SET polled_at = NULL")
        .execute(&server.pool)
        .await
        .unwrap();
    match poll_sign_in(&server.url(), poll).await.unwrap() {
        SignInStatus::Approved { token } => token,
        other => panic!("not approved: {other:?}"),
    }
}
