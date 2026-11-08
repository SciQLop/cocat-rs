mod support;

use cocat_client::{LoginError, login, rooms};
use support::*;

const ROOM: &str = "aaaaaaaa-1111-4111-8111-111111111111";

#[tokio::test]
async fn login_returns_the_session_cookie() {
    let server = Server::start().await;
    let _ = user(&server, "owner@x.org").await;

    let cookie = login(&server.url(), "owner@x.org", PASSWORD).await.unwrap();

    assert!(!cookie.is_empty());
}

#[tokio::test]
async fn a_bad_password_is_refused() {
    let server = Server::start().await;
    let _ = user(&server, "owner@x.org").await;

    let refused = login(&server.url(), "owner@x.org", "wrong").await;

    assert!(
        matches!(&refused, Err(LoginError::Refused { status: 400, detail }) if detail == "LOGIN_BAD_CREDENTIALS"),
        "{refused:?}"
    );
}

#[tokio::test]
async fn rooms_lists_the_callers_rooms_with_their_roles() {
    let server = Server::start().await;
    let owner = user(&server, "owner@x.org").await;
    room(&server, ROOM, owner).await;
    let cookie = login(&server.url(), "owner@x.org", PASSWORD).await.unwrap();

    let rooms = rooms(&server.url(), &cookie).await.unwrap();

    assert_eq!(rooms.len(), 1);
    assert_eq!(rooms[0].id, ROOM);
    assert_eq!(rooms[0].role, cocat_core::role::Role::Owner);
}

#[tokio::test]
async fn the_cookie_from_a_login_opens_the_room() {
    let server = Server::start().await;
    let owner = user(&server, "owner@x.org").await;
    room(&server, ROOM, owner).await;
    let cookie = login(&server.url(), "owner@x.org", PASSWORD).await.unwrap();

    let dir = cache();
    let room = cocat_client::Room::open(dir.path(), ROOM).unwrap();
    room.connect(&server.url(), &cookie).unwrap();

    room.wait_synced(WAIT).unwrap();
}

#[tokio::test]
async fn rooms_flags_the_callers_own_project() {
    let server = Server::start().await;
    let owner = user(&server, "owner@x.org").await;
    room(&server, ROOM, owner).await;
    cocat_server::db::create_personal_room(&server.pool, owner)
        .await
        .unwrap();
    let cookie = login(&server.url(), "owner@x.org", PASSWORD).await.unwrap();

    let rooms = rooms(&server.url(), &cookie).await.unwrap();

    let own: Vec<&str> = rooms
        .iter()
        .filter(|room| room.personal)
        .map(|room| room.id.as_str())
        .collect();
    assert_eq!(rooms.len(), 2);
    assert_ne!(own, [ROOM]);
    assert_eq!(own.len(), 1);
}
