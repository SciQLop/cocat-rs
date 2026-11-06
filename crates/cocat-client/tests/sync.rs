mod support;

use std::time::Duration;

use cocat_client::{Change, Room, Status};
use cocat_core::model::{self, NewCatalogue, NewEvent};
use cocat_core::role::Role;
use cocat_core::time::Timestamp;
use serde_json::Map;
use support::*;
use tokio::sync::broadcast::error::TryRecvError;
use tokio::time::timeout;

const ROOM: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const CATALOGUE: &str = "6f9c1a52-0b0d-4f4e-9d5e-1b2c3d4e5f60";
const FIRST: &str = "11111111-2222-4333-8444-555555555555";
const SECOND: &str = "22222222-3333-4444-8555-666666666666";
const E1: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const E2: &str = "bbbbbbbb-2222-4222-8222-222222222222";
const E3: &str = "cccccccc-3333-4333-8333-333333333333";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_rooms_converge() {
    let server = Server::start().await;
    let token = seeded(&server, ROOM, "owner@x.org").await;
    let a_dir = cache();
    let b_dir = cache();
    let a = Room::open(a_dir.path(), ROOM).unwrap();
    let b = Room::open(b_dir.path(), ROOM).unwrap();
    let mut changes = b.changes();

    a.connect(&server.url(), &token).unwrap();
    b.connect(&server.url(), &token).unwrap();
    a.wait_synced(WAIT).unwrap();
    b.wait_synced(WAIT).unwrap();

    create_catalogue(&a, CATALOGUE, "alpha");

    let batch = next_batch(&mut changes).await;
    assert!(
        batch
            .iter()
            .any(|change| matches!(change, Change::CatalogueAdded { uuid } if uuid == CATALOGUE)),
        "the reader got {batch:?}"
    );
    assert!(has_catalogue(&b, CATALOGUE));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_made_offline_reaches_the_server() {
    let server = Server::start().await;
    let token = seeded(&server, ROOM, "owner@x.org").await;
    let b_dir = cache();
    let b = Room::open(b_dir.path(), ROOM).unwrap();
    b.connect(&server.url(), &token).unwrap();
    b.wait_synced(WAIT).unwrap();
    let mut changes = b.changes();

    let a_dir = cache();
    let a = Room::open(a_dir.path(), ROOM).unwrap();
    create_catalogue(&a, CATALOGUE, "alpha");
    a.connect(&server.url(), &token).unwrap();
    a.wait_synced(WAIT).unwrap();

    let batch = next_batch(&mut changes).await;
    assert!(
        batch
            .iter()
            .any(|change| matches!(change, Change::CatalogueAdded { uuid } if uuid == CATALOGUE)),
        "the reader got {batch:?}"
    );
    assert!(has_catalogue(&b, CATALOGUE));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_reaches_the_room_after_a_server_restart() {
    let mut server = Server::start().await;
    let token = seeded(&server, ROOM, "owner@x.org").await;
    let a_dir = cache();
    let a = Room::open(a_dir.path(), ROOM).unwrap();
    a.connect(&server.url(), &token).unwrap();
    a.wait_synced(WAIT).unwrap();
    create_catalogue(&a, FIRST, "first");

    server.stop().await;
    a.wait_for(Status::Connecting, WAIT).unwrap();
    create_catalogue(&a, SECOND, "second");
    server.restart().await;
    a.wait_synced(WAIT).unwrap();

    let b_dir = cache();
    let b = Room::open(b_dir.path(), ROOM).unwrap();
    b.connect(&server.url(), &token).unwrap();
    b.wait_synced(WAIT).unwrap();

    assert_eq!(names(&b), ["first", "second"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_handshake_is_unauthorized_and_is_not_retried() {
    let refuser = Refuser::start().await;
    let dir = cache();
    let room = Room::open(dir.path(), ROOM).unwrap();

    room.connect(&refuser.url(), "a-stale-cookie").unwrap();
    room.wait_for(Status::Unauthorized, WAIT).unwrap();
    assert_eq!(refuser.hits.load(std::sync::atomic::Ordering::SeqCst), 1);

    // The wait is the assertion: a retry loop would knock again within one backoff.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(room.status(), Status::Unauthorized);
    assert_eq!(
        refuser.hits.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the client kept retrying a refused handshake"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_server_drops_a_viewer_write() {
    let server = Server::start().await;
    let owner = user(&server, "owner@x.org").await;
    room(&server, ROOM, owner).await;
    let viewer = user(&server, "viewer@x.org").await;
    member(&server, ROOM, viewer, Role::Viewer).await;
    let owner_token = token(&server, owner).await;
    let viewer_token = token(&server, viewer).await;

    let a_dir = cache();
    let a = Room::open(a_dir.path(), ROOM).unwrap();
    a.connect(&server.url(), &owner_token).unwrap();
    a.wait_synced(WAIT).unwrap();
    let mut changes = a.changes();

    let v_dir = cache();
    let v = Room::open(v_dir.path(), ROOM).unwrap();
    v.connect(&server.url(), &viewer_token).unwrap();
    v.wait_synced(WAIT).unwrap();

    create_catalogue(&v, CATALOGUE, "sneaky");

    // The server ignores a viewer's update without a word, so the client has nothing to
    // report: it stays synced and the write never leaves this room.
    assert_eq!(v.status(), Status::Synced);
    assert!(
        timeout(Duration::from_millis(500), changes.recv())
            .await
            .is_err(),
        "the server forwarded a viewer's write"
    );
    assert!(!has_catalogue(&a, CATALOGUE));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_remote_transaction_is_one_merged_batch() {
    let server = Server::start().await;
    let token = seeded(&server, ROOM, "owner@x.org").await;
    let a_dir = cache();
    let b_dir = cache();
    let a = Room::open(a_dir.path(), ROOM).unwrap();
    let b = Room::open(b_dir.path(), ROOM).unwrap();
    let mut changes = b.changes();
    a.connect(&server.url(), &token).unwrap();
    b.connect(&server.url(), &token).unwrap();
    a.wait_synced(WAIT).unwrap();
    b.wait_synced(WAIT).unwrap();

    let tags = Vec::new();
    let attributes = Map::new();
    a.write(|txn| {
        model::create_catalogue(
            txn,
            &NewCatalogue {
                uuid: CATALOGUE,
                name: "alpha",
                author: "a@x.org",
                tags: &tags,
                attributes: &attributes,
            },
        )
        .unwrap();
        for uuid in [E1, E2, E3] {
            model::create_event(
                txn,
                &NewEvent {
                    uuid,
                    start: Timestamp::parse("2025-01-31 10:00:00").unwrap(),
                    stop: Timestamp::parse("2025-01-31 10:01:00").unwrap(),
                    author: "a@x.org",
                    tags: &tags,
                    products: &tags,
                    rating: None,
                    attributes: &attributes,
                },
            )
            .unwrap();
        }
        model::add_catalogue_events(
            txn,
            CATALOGUE,
            &[E1.to_owned(), E2.to_owned(), E3.to_owned()],
        );
    });

    let batch = next_batch(&mut changes).await;
    assert_eq!(
        batch.len(),
        2,
        "one transaction is one batch of two merged changes: {batch:?}"
    );
    assert!(
        batch
            .iter()
            .any(|change| matches!(change, Change::CatalogueAdded { uuid } if uuid == CATALOGUE))
    );
    assert!(
        batch.iter().any(|change| matches!(
            change,
            Change::EventsAdded { catalogue, uuids }
                if catalogue == CATALOGUE && uuids == &[E1.to_owned(), E2.to_owned(), E3.to_owned()]
        )),
        "the three events are one change: {batch:?}"
    );
    assert!(matches!(changes.try_recv(), Err(TryRecvError::Empty)));
}
