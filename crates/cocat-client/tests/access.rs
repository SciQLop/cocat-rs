//! What a room does when the server says its account's access to the project changed.

mod support;

use std::time::Duration;

use cocat_client::{Credentials, Room, RoomError, Status};
use cocat_core::role::Role;
use cocat_server::{db, lifecycle};
use support::*;

const ROOM: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const CATALOGUE: &str = "6f9c1a52-0b0d-4f4e-9d5e-1b2c3d4e5f60";
/// Well under the one-second first backoff: a changed access reconnects without waiting.
const AT_ONCE: Duration = Duration::from_millis(700);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_downgraded_editor_reconnects_as_a_viewer() {
    let server = Server::start().await;
    let owner = user(&server, "owner@x.org").await;
    room(&server, ROOM, owner).await;
    let editor = user(&server, "editor@x.org").await;
    member(&server, ROOM, editor, Role::Editor).await;
    let dir = cache();
    let client = Room::open(dir.path(), ROOM).unwrap();
    client
        .connect(&server.url(), &token(&server, editor).await)
        .unwrap();
    client.wait_synced(WAIT).unwrap();
    assert_eq!(client.role(), Some(Role::Editor));

    lifecycle::set_member_role(server.state(), ROOM, owner, editor, Role::Viewer)
        .await
        .unwrap();

    wait_until(AT_ONCE, || client.role() == Some(Role::Viewer)).await;
    client.wait_synced(WAIT).unwrap();
    assert!(client.is_read_only());
    assert!(matches!(
        client.write(|txn| cocat_core::model::create_catalogue(
            txn,
            &new_catalogue(CATALOGUE, "late")
        )),
        Err(RoomError::ReadOnly(Role::Viewer))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_removed_member_has_no_access_and_does_not_retry() {
    let server = Server::start().await;
    let owner = user(&server, "owner@x.org").await;
    room(&server, ROOM, owner).await;
    let editor = user(&server, "editor@x.org").await;
    member(&server, ROOM, editor, Role::Editor).await;
    let dir = cache();
    let client = Room::open(dir.path(), ROOM).unwrap();
    client
        .connect_with(
            &server.url(),
            &token(&server, editor).await,
            Some(Credentials::new("editor@x.org", PASSWORD)),
        )
        .unwrap();
    client.wait_synced(WAIT).unwrap();

    lifecycle::remove_member(server.state(), ROOM, owner, editor)
        .await
        .unwrap();

    client.wait_for(Status::NoAccess, WAIT).unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(client.status(), Status::NoAccess);
    assert_eq!(
        server.logins.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "losing a project is not a reason to sign in again"
    );
    assert!(matches!(
        client.wait_synced(Duration::from_millis(10)),
        Err(RoomError::NoAccess)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ended_session_is_still_unauthorized() {
    let server = Server::start().await;
    let owner = user(&server, "owner@x.org").await;
    room(&server, ROOM, owner).await;
    let dir = cache();
    let client = Room::open(dir.path(), ROOM).unwrap();
    client
        .connect(&server.url(), &token(&server, owner).await)
        .unwrap();
    client.wait_synced(WAIT).unwrap();

    for hash in db::end_sessions_of(&server.pool, owner).await.unwrap() {
        server.state().rooms.end_session(&hash);
    }

    client.wait_for(Status::Unauthorized, WAIT).unwrap();
}

/// Polls a condition the room reaches on its own sync thread.
async fn wait_until(within: Duration, reached: impl Fn() -> bool) {
    tokio::time::timeout(within, async {
        while !reached() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the room never got there");
}
