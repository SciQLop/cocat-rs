use std::fs;
use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use cocat_core::role::Role;
use cocat_core::store::{FileRoomStore, HEADER};
use cocat_server::room::{
    ClientId, CloseReason, Inbox, OUTBOUND_FRAMES, RoomHandle, RoomManager, outbox,
};
use tempfile::TempDir;
use tokio::time::timeout;
use uuid::Uuid;
use yrs::sync::{Message, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, Map, ReadTxn, StateVector, Transact};

const WAIT: Duration = Duration::from_secs(10);

fn manager() -> (TempDir, RoomManager) {
    let dir = TempDir::new().unwrap();
    let manager = RoomManager::new(dir.path().to_owned());
    (dir, manager)
}

fn update_with(key: &str, value: &str) -> Vec<u8> {
    let doc = Doc::new();
    doc.get_or_insert_map("content")
        .insert(&mut doc.transact_mut(), key, value);
    doc.transact()
        .encode_state_as_update_v1(&StateVector::default())
}

fn sync_step1() -> Bytes {
    Bytes::from(Message::Sync(SyncMessage::SyncStep1(StateVector::default())).encode_v1())
}

fn update_frame(payload: &[u8]) -> Bytes {
    Bytes::from(Message::Sync(SyncMessage::Update(payload.to_vec())).encode_v1())
}

fn attach_as(handle: &RoomHandle, client: ClientId, user: Uuid, role: Role) -> Inbox {
    let (out, inbox) = outbox();
    handle
        .attach(client, user, role, out)
        .expect("the room accepted the client");
    inbox
}

fn attach(handle: &RoomHandle, client: ClientId, role: Role) -> Inbox {
    attach_as(handle, client, Uuid::new_v4(), role)
}

fn attach_with_retry(
    manager: &RoomManager,
    room_id: &str,
    client: ClientId,
    role: Role,
) -> (RoomHandle, Inbox) {
    for _ in 0..3 {
        let handle = manager.get_or_open(room_id).unwrap();
        let (out, inbox) = outbox();
        if handle.attach(client, Uuid::new_v4(), role, out).is_ok() {
            return (handle, inbox);
        }
    }
    panic!("the room never accepted client {client}");
}

async fn next(inbox: &mut Inbox) -> Message {
    let frame = timeout(WAIT, inbox.frames.recv())
        .await
        .expect("the room went quiet")
        .expect("the room closed the channel");
    Message::decode_v1(&frame).expect("the room sent a bad frame")
}

fn content(doc: &Doc, key: &str) -> Option<String> {
    doc.get_or_insert_map("content")
        .get(&doc.transact(), key)
        .map(|value| value.to_string(&doc.transact()))
}

fn apply(payload: &[u8]) -> Doc {
    let doc = Doc::new();
    doc.transact_mut()
        .apply_update(yrs::Update::decode_v1(payload).unwrap())
        .unwrap();
    doc
}

#[tokio::test]
async fn a_sync_step1_gets_a_step2() {
    let (_dir, manager) = manager();
    let room = manager.get_or_open("room-step").unwrap();
    let mut client = attach(&room, 1, Role::Editor);

    assert!(matches!(
        next(&mut client).await,
        Message::Sync(SyncMessage::SyncStep1(_))
    ));

    room.frame(1, sync_step1());

    assert!(matches!(
        next(&mut client).await,
        Message::Sync(SyncMessage::SyncStep2(_))
    ));
}

#[tokio::test]
async fn an_update_reaches_every_attached_client() {
    let (_dir, manager) = manager();
    let room = manager.get_or_open("room-broadcast").unwrap();
    let mut first = attach(&room, 1, Role::Editor);
    let mut second = attach(&room, 2, Role::Editor);
    next(&mut first).await;
    next(&mut second).await;

    let payload = update_with("key", "value");
    room.frame(1, update_frame(&payload));

    for frames in [&mut first, &mut second] {
        match next(frames).await {
            Message::Sync(SyncMessage::Update(update)) => assert_eq!(update, payload),
            other => panic!("expected an update, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_viewer_sync_step2_is_dropped_but_their_step1_is_answered() {
    let (_dir, manager) = manager();
    let room = manager.get_or_open("room-viewer").unwrap();
    let mut viewer = attach(&room, 1, Role::Viewer);
    next(&mut viewer).await;

    let payload = update_with("key", "value");
    room.frame(
        1,
        Bytes::from(Message::Sync(SyncMessage::SyncStep2(payload)).encode_v1()),
    );
    room.frame(1, sync_step1());

    let reply = match next(&mut viewer).await {
        Message::Sync(SyncMessage::SyncStep2(update)) => update,
        other => panic!("expected a step2, got {other:?}"),
    };
    assert_eq!(content(&apply(&reply), "key"), None);
}

#[tokio::test]
async fn two_clients_updates_both_land_and_both_are_broadcast() {
    let (dir, manager) = manager();
    let room = manager.get_or_open("room-two").unwrap();
    let mut first = attach(&room, 1, Role::Editor);
    let mut second = attach(&room, 2, Role::Editor);
    next(&mut first).await;
    next(&mut second).await;

    let alpha = update_with("alpha", "1");
    let beta = update_with("beta", "2");
    room.frame(1, update_frame(&alpha));
    room.frame(2, update_frame(&beta));

    for frames in [&mut first, &mut second] {
        let mut seen = Vec::new();
        for _ in 0..2 {
            match next(frames).await {
                Message::Sync(SyncMessage::Update(update)) => seen.push(update),
                other => panic!("expected an update, got {other:?}"),
            }
        }
        assert_eq!(seen, [alpha.clone(), beta.clone()]);
    }

    room.detach(1).await;
    room.detach(2).await;

    let (_, updates) = FileRoomStore::open(dir.path(), "room-two").unwrap();
    assert_eq!(updates, [Bytes::from(alpha), Bytes::from(beta)]);
}

#[tokio::test]
async fn frames_shorter_than_two_bytes_are_ignored() {
    let (_dir, manager) = manager();
    let room = manager.get_or_open("room-short").unwrap();
    let mut client = attach(&room, 1, Role::Editor);
    next(&mut client).await;

    room.frame(1, Bytes::new());
    room.frame(1, Bytes::from_static(b"\x00"));
    room.frame(1, sync_step1());

    assert!(matches!(
        next(&mut client).await,
        Message::Sync(SyncMessage::SyncStep2(_))
    ));
}

#[tokio::test]
async fn the_last_detach_flushes_and_unloads_the_room() {
    let (dir, manager) = manager();
    let room = manager.get_or_open("room-detach").unwrap();
    let mut client = attach(&room, 1, Role::Editor);
    next(&mut client).await;

    let payload = update_with("key", "value");
    room.frame(1, update_frame(&payload));
    next(&mut client).await;

    room.detach(1).await;

    assert_eq!(manager.open_rooms(), 0);
    let (_, updates) = FileRoomStore::open(dir.path(), "room-detach").unwrap();
    assert_eq!(updates, [Bytes::from(payload)]);
}

#[tokio::test]
async fn reopening_a_room_replays_the_file() {
    let (_dir, manager) = manager();
    let room = manager.get_or_open("room-replay").unwrap();
    let mut client = attach(&room, 1, Role::Editor);
    next(&mut client).await;
    room.frame(1, update_frame(&update_with("key", "value")));
    next(&mut client).await;
    room.detach(1).await;

    let reopened = manager.get_or_open("room-replay").unwrap();
    let mut viewer = attach(&reopened, 2, Role::Viewer);
    next(&mut viewer).await;
    reopened.frame(2, sync_step1());

    let reply = match next(&mut viewer).await {
        Message::Sync(SyncMessage::SyncStep2(update)) => update,
        other => panic!("expected a step2, got {other:?}"),
    };
    assert_eq!(content(&apply(&reply), "key").as_deref(), Some("value"));
}

#[tokio::test]
async fn an_undecodable_record_is_skipped_at_open() {
    let (dir, manager) = manager();
    let payload = update_with("key", "value");
    let mut file = HEADER.to_vec();
    file.push(4);
    file.extend_from_slice(b"junk");
    file.push(payload.len() as u8);
    file.extend_from_slice(&payload);
    fs::write(Path::new(dir.path()).join("room-garbage.y"), file).unwrap();

    let room = manager.get_or_open("room-garbage").unwrap();
    let mut client = attach(&room, 1, Role::Viewer);
    next(&mut client).await;
    room.frame(1, sync_step1());

    let reply = match next(&mut client).await {
        Message::Sync(SyncMessage::SyncStep2(update)) => update,
        other => panic!("expected a step2, got {other:?}"),
    };
    assert_eq!(content(&apply(&reply), "key").as_deref(), Some("value"));
}

#[tokio::test]
async fn an_editor_update_is_on_disk_while_the_client_is_attached() {
    let (dir, manager) = manager();
    let room = manager.get_or_open("room-durable").unwrap();
    let mut client = attach(&room, 1, Role::Editor);
    next(&mut client).await;
    let mut reader = attach(&room, 2, Role::Viewer);
    next(&mut reader).await;

    let payload = update_with("key", "value");
    room.frame(1, update_frame(&payload));
    next(&mut reader).await;

    let (_, updates) = FileRoomStore::open(dir.path(), "room-durable").unwrap();
    assert_eq!(updates, [Bytes::from(payload)]);
}

#[tokio::test]
async fn an_empty_update_is_not_written_or_broadcast() {
    let (dir, manager) = manager();
    let room = manager.get_or_open("room-empty").unwrap();
    let mut client = attach(&room, 1, Role::Editor);
    next(&mut client).await;

    room.frame(
        1,
        Bytes::from(Message::Sync(SyncMessage::SyncStep2(vec![0, 0])).encode_v1()),
    );
    room.frame(1, sync_step1());

    assert!(matches!(
        next(&mut client).await,
        Message::Sync(SyncMessage::SyncStep2(_))
    ));
    let path = dir.path().join("room-empty.y");
    assert_eq!(fs::metadata(path).unwrap().len(), HEADER.len() as u64);
}

#[tokio::test]
async fn a_client_whose_queue_is_full_is_dropped_with_too_slow() {
    let (_dir, manager) = manager();
    let room = manager.get_or_open("room-full").unwrap();
    let silent = attach(&room, 1, Role::Editor);
    let mut reader = attach(&room, 2, Role::Editor);
    next(&mut reader).await;

    let payload = update_with("key", "value");
    for _ in 0..OUTBOUND_FRAMES + 1 {
        room.frame(2, update_frame(&payload));
        // The sender must keep reading or it overflows too: awaiting each broadcast
        // keeps its queue at one frame, so only the silent client is dropped.
        next(&mut reader).await;
    }

    let reason = timeout(WAIT, silent.close)
        .await
        .expect("the room never closed the silent client")
        .expect("the room dropped the close signal");
    assert_eq!(reason, CloseReason::TooSlow);
    room.detach(2).await;
}

#[tokio::test]
async fn frames_from_a_dropped_client_are_ignored() {
    let (_dir, manager) = manager();
    let room = manager.get_or_open("room-dropped").unwrap();
    let silent = attach(&room, 1, Role::Editor);
    let mut reader = attach(&room, 2, Role::Editor);
    next(&mut reader).await;

    let payload = update_with("key", "value");
    for _ in 0..OUTBOUND_FRAMES + 1 {
        room.frame(2, update_frame(&payload));
        next(&mut reader).await;
    }
    timeout(WAIT, silent.close)
        .await
        .expect("the room never dropped the silent client")
        .expect("the room never sent a close reason");

    // The dropped client keeps talking: nothing of it may reach the document, and a frame
    // must not re-attach it.
    room.frame(1, update_frame(&update_with("dropped", "yes")));
    room.frame(1, sync_step1());
    room.frame(2, sync_step1());

    let reply = match next(&mut reader).await {
        Message::Sync(SyncMessage::SyncStep2(update)) => update,
        other => panic!("expected a step2, got {other:?}"),
    };
    let doc = apply(&reply);
    assert_eq!(content(&doc, "dropped"), None);
    assert_eq!(content(&doc, "key").as_deref(), Some("value"));
    room.detach(2).await;
}

#[tokio::test]
async fn a_detach_and_an_attach_racing_keep_the_room_serving() {
    let (_dir, manager) = manager();
    for _ in 0..200 {
        let (room, mut first) = attach_with_retry(&manager, "room-race", 1, Role::Editor);
        next(&mut first).await;

        let detaching = tokio::spawn(async move { room.detach(1).await });
        let (room, mut second) = attach_with_retry(&manager, "room-race", 2, Role::Editor);

        assert!(matches!(
            next(&mut second).await,
            Message::Sync(SyncMessage::SyncStep1(_))
        ));
        detaching.await.unwrap();
        room.detach(2).await;
    }
}

#[tokio::test]
async fn revoke_closes_only_that_users_clients() {
    let (_dir, manager) = manager();
    let room = manager.get_or_open("room-revoke").unwrap();
    let revoked = Uuid::new_v4();
    let mut first = attach_as(&room, 1, revoked, Role::Editor);
    let mut second = attach_as(&room, 2, revoked, Role::Editor);
    let mut kept = attach_as(&room, 3, Uuid::new_v4(), Role::Editor);
    for inbox in [&mut first, &mut second, &mut kept] {
        next(inbox).await;
    }

    manager.revoke("room-revoke", revoked).await;

    for inbox in [&mut first, &mut second] {
        let reason = timeout(WAIT, &mut inbox.close)
            .await
            .expect("the room never closed a revoked client")
            .expect("the room dropped the close signal");
        assert_eq!(reason, CloseReason::Revoked);
    }
    room.frame(3, update_frame(&update_with("key", "value")));
    assert!(matches!(
        next(&mut kept).await,
        Message::Sync(SyncMessage::Update(_))
    ));
    room.detach(3).await;
}

#[tokio::test]
async fn revoke_on_a_closed_room_is_a_no_op() {
    let (dir, manager) = manager();

    manager.revoke("room-closed", Uuid::new_v4()).await;

    assert_eq!(manager.open_rooms(), 0);
    assert!(!dir.path().join("room-closed.y").exists());
}

#[tokio::test]
async fn a_deleted_room_does_not_resurrect_its_file() {
    let (dir, manager) = manager();
    let room = manager.get_or_open("room-delete").unwrap();
    let mut client = attach(&room, 1, Role::Editor);
    next(&mut client).await;
    room.frame(1, update_frame(&update_with("key", "value")));
    next(&mut client).await;
    assert!(dir.path().join("room-delete.y").exists());

    manager.delete("room-delete").await;

    assert!(!dir.path().join("room-delete.y").exists());
    assert_eq!(manager.open_rooms(), 0);
    let reason = timeout(WAIT, client.close)
        .await
        .expect("the room never closed the deleted client")
        .expect("the room dropped the close signal");
    assert_eq!(reason, CloseReason::Revoked);

    let reopened = manager.get_or_open("room-delete").unwrap();
    let mut viewer = attach(&reopened, 2, Role::Viewer);
    next(&mut viewer).await;
    reopened.frame(2, sync_step1());
    let reply = match next(&mut viewer).await {
        Message::Sync(SyncMessage::SyncStep2(update)) => update,
        other => panic!("expected a step2, got {other:?}"),
    };
    assert_eq!(content(&apply(&reply), "key"), None);
    reopened.detach(2).await;
}
