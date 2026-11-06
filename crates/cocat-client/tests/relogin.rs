//! What a room does when the server refuses its cookie and it holds credentials.

mod support;

use std::sync::atomic::Ordering;
use std::time::Duration;

use cocat_client::{Credentials, Room, Status};
use cocat_server::db;
use support::*;
use uuid::Uuid;

const ROOM: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const CATALOGUE: &str = "6f9c1a52-0b0d-4f4e-9d5e-1b2c3d4e5f60";

async fn sessions(server: &Server, user: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM session WHERE user_id = ?")
        .bind(user.to_string())
        .fetch_one(&server.pool)
        .await
        .unwrap()
}

fn credentials() -> Option<Credentials> {
    Some(Credentials::new("owner@x.org", PASSWORD))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoked_session_logs_in_again_and_syncs() {
    let mut server = Server::start().await;
    let owner = user(&server, "owner@x.org").await;
    room(&server, ROOM, owner).await;
    let token = token(&server, owner).await;
    let dir = cache();
    let client = Room::open(dir.path(), ROOM).unwrap();
    client
        .connect_with(&server.url(), &token, credentials())
        .unwrap();
    client.wait_synced(WAIT).unwrap();

    db::end_sessions_of(&server.pool, owner).await.unwrap();
    assert_eq!(sessions(&server, owner).await, 0);
    server.restart().await;
    client.wait_for(Status::Connecting, WAIT).unwrap();
    client.wait_synced(WAIT).unwrap();

    assert_eq!(
        sessions(&server, owner).await,
        1,
        "only a login can make a session row again"
    );
    create_catalogue(&client, CATALOGUE, "after");
    client.save(WAIT).unwrap();
    assert!(has_catalogue(&client, CATALOGUE));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wrong_password_stops_after_one_login() {
    let server = Server::start().await;
    let owner = user(&server, "owner@x.org").await;
    room(&server, ROOM, owner).await;
    let token = token(&server, owner).await;
    db::end_sessions_of(&server.pool, owner).await.unwrap();
    let dir = cache();
    let client = Room::open(dir.path(), ROOM).unwrap();

    client
        .connect_with(
            &server.url(),
            &token,
            Some(Credentials::new("owner@x.org", "wrong-password")),
        )
        .unwrap();
    client.wait_for(Status::Unauthorized, WAIT).unwrap();
    assert_eq!(server.logins.load(Ordering::SeqCst), 1);

    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(client.status(), Status::Unauthorized);
    assert_eq!(
        server.logins.load(Ordering::SeqCst),
        1,
        "the client hammered the login route"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_credentials_a_refusal_is_still_final() {
    let refuser = Refuser::start().await;
    let dir = cache();
    let client = Room::open(dir.path(), ROOM).unwrap();

    client.connect(&refuser.url(), "a-stale-cookie").unwrap();
    client.wait_for(Status::Unauthorized, WAIT).unwrap();

    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(refuser.hits.load(Ordering::SeqCst), 1);
}
