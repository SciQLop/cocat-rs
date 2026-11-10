mod support;

use std::time::Duration;

use cocat_client::{Change, Room, RoomError, Status};
use cocat_core::model;
use cocat_core::role::Role;
use cocat_core::time::Timestamp;
use support::*;
use tokio::sync::broadcast::error::TryRecvError;
use tokio::time::timeout;
use yrs::{ReadTxn, StateVector, Transact};

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
async fn a_viewer_write_is_refused_and_never_sent() {
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

    let refused = v.write(|txn| model::create_catalogue(txn, &new_catalogue(CATALOGUE, "sneaky")));

    assert!(matches!(refused, Err(RoomError::ReadOnly(Role::Viewer))));
    assert!(!has_catalogue(&v, CATALOGUE));
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
async fn a_synced_room_opens_offline_from_its_cache() {
    let mut server = Server::start().await;
    let token = seeded(&server, ROOM, "owner@x.org").await;
    let a_dir = cache();
    let a = Room::open(a_dir.path(), ROOM).unwrap();
    a.connect(&server.url(), &token).unwrap();
    a.wait_synced(WAIT).unwrap();
    create_catalogue(&a, CATALOGUE, "alpha");

    let b_dir = cache();
    let b = Room::open(b_dir.path(), ROOM).unwrap();
    let mut changes = b.changes();
    b.connect(&server.url(), &token).unwrap();
    b.wait_synced(WAIT).unwrap();
    next_batch(&mut changes).await;
    assert!(has_catalogue(&b, CATALOGUE));
    b.close();
    drop(b);
    server.stop().await;

    let offline = Room::open(b_dir.path(), ROOM).unwrap();
    assert_eq!(offline.status(), Status::Offline);
    assert_eq!(
        names(&offline),
        ["alpha"],
        "the cache holds only what the room still had to send"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reconnect_downloads_only_the_diff() {
    let server = Server::start().await;
    let token = seeded(&server, ROOM, "owner@x.org").await;
    let a_dir = cache();
    let a = Room::open(a_dir.path(), ROOM).unwrap();
    a.connect(&server.url(), &token).unwrap();
    a.wait_synced(WAIT).unwrap();

    let events: Vec<String> = (0..1000).map(|n| format!("event-{n}")).collect();
    write_catalogue_with_events(&a, CATALOGUE, &events);

    let b_dir = cache();
    let b = Room::open(b_dir.path(), ROOM).unwrap();
    b.connect(&server.url(), &token).unwrap();
    b.wait_synced(WAIT).unwrap();
    assert_eq!(listed(&b, CATALOGUE), 1000);
    b.close();
    drop(b);

    // A witness proves the server has the change before the measurement runs, so a zero
    // difference means the cache was not reused rather than a race.
    let witness_dir = cache();
    let witness = Room::open(witness_dir.path(), ROOM).unwrap();
    let mut changes = witness.changes();
    witness.connect(&server.url(), &token).unwrap();
    witness.wait_synced(WAIT).unwrap();

    a.write(|txn| {
        model::set_event_range(
            txn,
            &events[0],
            Timestamp::parse("2025-02-01 00:00:00").unwrap(),
            Timestamp::parse("2025-02-01 00:01:00").unwrap(),
        )
        .unwrap()
    })
    .unwrap();
    // The first batch is the whole room arriving; the one after it carries the change, which
    // proves the server applied it before the measurement runs.
    loop {
        let batch = next_batch(&mut changes).await;
        if batch
            .iter()
            .any(|change| matches!(change, Change::EventRange { uuid, .. } if uuid == &events[0]))
        {
            break;
        }
    }

    let cached = Room::open(b_dir.path(), ROOM).unwrap();
    let state = cached.doc().transact().state_vector();
    let full = sync_step2_bytes(server.addr, &token, ROOM, StateVector::default()).await;
    let diff = sync_step2_bytes(server.addr, &token, ROOM, state).await;
    println!("full state {full} bytes, reconnect after one change {diff} bytes");
    assert!(diff > 0, "the reconnect asked for nothing: {diff} bytes");
    assert!(
        diff * 20 < full,
        "the reconnect downloaded {diff} of {full} bytes"
    );
}

fn write_catalogue_with_events(room: &Room, catalogue: &str, events: &[String]) {
    room.write(|txn| {
        model::create_catalogue(txn, &new_catalogue(catalogue, "alpha")).unwrap();
        for uuid in events {
            let event = new_event(uuid, "2025-01-31 10:00:00", "2025-01-31 10:01:00");
            model::create_event(txn, &event).unwrap();
        }
        model::add_catalogue_events(txn, catalogue, events);
    })
    .unwrap();
}

fn listed(room: &Room, catalogue: &str) -> usize {
    let doc = room.doc();
    let txn = doc.transact();
    model::catalogue_events(&txn, catalogue)
        .and_then(Result::ok)
        .map_or(0, |events| events.len())
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

    a.write(|txn| {
        model::create_catalogue(txn, &new_catalogue(CATALOGUE, "alpha")).unwrap();
        for uuid in [E1, E2, E3] {
            let event = new_event(uuid, "2025-01-31 10:00:00", "2025-01-31 10:01:00");
            model::create_event(txn, &event).unwrap();
        }
        model::add_catalogue_events(
            txn,
            CATALOGUE,
            &[E1.to_owned(), E2.to_owned(), E3.to_owned()],
        );
    })
    .unwrap();

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

/// Deployed servers sit behind https: the room must open a TLS websocket, not give up on `wss`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_https_url_opens_a_tls_connection() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let dir = cache();
    let room = Room::open(dir.path(), ROOM).unwrap();

    room.connect(&format!("https://127.0.0.1:{port}"), "fastapiusersauth=x")
        .unwrap();

    let (mut stream, _) = timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("the room connects")
        .unwrap();
    let mut first = [0u8; 1];
    tokio::io::AsyncReadExt::read_exact(&mut stream, &mut first)
        .await
        .unwrap();
    const TLS_HANDSHAKE_RECORD: u8 = 0x16;
    assert_eq!(first[0], TLS_HANDSHAKE_RECORD);
    room.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_viewer_drops_unshared_changes_and_writes_nothing() {
    let server = Server::start().await;
    let owner_token = seeded(&server, ROOM, "owner@x.org").await;
    let viewer = user(&server, "viewer@x.org").await;
    member(&server, ROOM, viewer, Role::Viewer).await;
    let owner_dir = cache();
    let owner = Room::open(owner_dir.path(), ROOM).unwrap();
    owner.connect(&server.url(), &owner_token).unwrap();
    owner.wait_synced(WAIT).unwrap();
    create_catalogue(&owner, FIRST, "shared");
    owner.save(WAIT).unwrap();

    let dir = cache();
    let drifted = Room::open(dir.path(), ROOM).unwrap();
    create_catalogue(&drifted, SECOND, "phantom");
    drifted.close();
    drop(drifted);

    let room = Room::open(dir.path(), ROOM).unwrap();
    room.connect(&server.url(), &token(&server, viewer).await)
        .unwrap();
    room.wait_synced(WAIT).unwrap();

    assert_eq!(room.role(), Some(Role::Viewer));
    assert_eq!(names(&room), ["shared"]);
    assert!(matches!(
        room.write(|_| ()),
        Err(RoomError::ReadOnly(Role::Viewer))
    ));
    room.close();
    let reopened = Room::open(dir.path(), ROOM).unwrap();
    assert!(reopened.is_read_only());
    assert_eq!(names(&reopened), ["shared"]);
}

/// A relayed catalogue such as LASCO's CMEs holds about 20 MiB, more than the 16 MiB a
/// websocket message may carry by default.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_room_larger_than_16_mib_opens() {
    let server = Server::start().await;
    let token = seeded(&server, ROOM, "owner@x.org").await;
    let mut big = new_event(E1, "2024-05-14T16:46:00Z", "2024-05-14T17:08:00Z");
    big.attributes
        .insert("remarks".into(), "x".repeat(20 << 20).into());
    let doc = yrs::Doc::new();
    let mut txn = doc.transact_mut();
    model::create_event(&mut txn, &big).unwrap();
    let update = txn.encode_update_v1();
    server
        .state()
        .rooms
        .write_as(ROOM, u64::MAX, cocat_server::room::Author::Server, update)
        .await
        .unwrap();

    let dir = cache();
    let room = Room::open(dir.path(), ROOM).unwrap();
    room.connect(&server.url(), &token).unwrap();
    room.wait_synced(WAIT).unwrap();

    assert!(model::event(&room.doc().transact(), E1).is_some());
}
