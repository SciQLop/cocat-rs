use std::fs;
use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use cocat_core::role::Role;
use cocat_core::store::{FileRoomStore, HEADER};
use cocat_server::room::{ClientId, RoomHandle, RoomManager};
use tempfile::TempDir;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::timeout;
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

fn attach(handle: &RoomHandle, client: ClientId, role: Role) -> UnboundedReceiver<Bytes> {
    let (out, frames) = tokio::sync::mpsc::unbounded_channel();
    handle.attach(client, role, out);
    frames
}

async fn next(frames: &mut UnboundedReceiver<Bytes>) -> Message {
    let frame = timeout(WAIT, frames.recv())
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

    room.detach(1);
    room.detach(2);

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

    room.detach(1);

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
    room.detach(1);

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
