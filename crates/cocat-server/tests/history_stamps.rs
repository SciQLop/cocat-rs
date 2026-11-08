use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use cocat_core::role::Role;
use cocat_core::store::{FileRoomStore, read_updates, room_file};
use cocat_server::history::stamps::{read_stamps, stamps_file, valid_stamps};
use cocat_server::room::{Inbox, RoomHandle, RoomManager, outbox};
use tempfile::TempDir;
use uuid::Uuid;
use yrs::sync::{Message, SyncMessage};
use yrs::updates::encoder::Encode;
use yrs::{Doc, Map, ReadTxn, StateVector, Transact};

fn update_with(key: &str, value: &str) -> Vec<u8> {
    let doc = Doc::new();
    doc.get_or_insert_map("content")
        .insert(&mut doc.transact_mut(), key, value);
    doc.transact()
        .encode_state_as_update_v1(&StateVector::default())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Attaches `user` as an editor, sends one update and waits for the room to flush it.
async fn write_as(room: &RoomHandle, user: Uuid, payload: &[u8]) -> Inbox {
    let (out, inbox) = outbox();
    room.attach(1, user, Role::Editor, out).unwrap();
    room.frame(
        1,
        Bytes::from(Message::Sync(SyncMessage::Update(payload.to_vec())).encode_v1()),
    );
    room.flush().await;
    inbox
}

#[tokio::test]
async fn an_update_from_a_client_is_stamped_with_its_user() {
    let dir = TempDir::new().unwrap();
    let manager = RoomManager::new(dir.path().to_owned(), None);
    let user = Uuid::new_v4();
    let before = now_ms();

    let room = manager.get_or_open("lab").unwrap();
    let _inbox = write_as(&room, user, &update_with("key", "value")).await;

    let stamps = read_stamps(dir.path(), "lab").unwrap();
    assert_eq!(stamps.len(), 1);
    let stamp = stamps[0];
    assert_eq!(stamp.record, 0);
    assert_eq!(stamp.user, Some(user));
    assert!((before..=now_ms()).contains(&stamp.at_ms));
    let records = read_updates(dir.path(), "lab").unwrap();
    assert_eq!(valid_stamps(&records, stamps), [Some(stamp)]);
}

#[tokio::test]
async fn a_room_file_swapped_underneath_drops_its_stamps() {
    let dir = TempDir::new().unwrap();
    let manager = RoomManager::new(dir.path().to_owned(), None);
    let room = manager.get_or_open("lab").unwrap();
    let _inbox = write_as(&room, Uuid::new_v4(), &update_with("key", "value")).await;
    let other = TempDir::new().unwrap();
    let (mut store, _) = FileRoomStore::open(other.path(), "lab").unwrap();
    store
        .append(Bytes::from(update_with("other", "file")))
        .unwrap();
    store.flush().unwrap();

    fs::copy(room_file(other.path(), "lab"), room_file(dir.path(), "lab")).unwrap();

    let records = read_updates(dir.path(), "lab").unwrap();
    let stamps = read_stamps(dir.path(), "lab").unwrap();
    assert_eq!(stamps.len(), 1);
    assert_eq!(valid_stamps(&records, stamps), [None]);
}

#[tokio::test]
async fn deleting_the_room_removes_its_stamps() {
    let dir = TempDir::new().unwrap();
    let manager = RoomManager::new(dir.path().to_owned(), None);
    let room = manager.get_or_open("lab").unwrap();
    let _inbox = write_as(&room, Uuid::new_v4(), &update_with("key", "value")).await;
    assert!(stamps_file(dir.path(), "lab").exists());

    manager.delete("lab").await;

    assert!(!stamps_file(dir.path(), "lab").exists());
    let user = Uuid::new_v4();
    let room = manager.get_or_open("lab").unwrap();
    let _inbox = write_as(&room, user, &update_with("new", "room")).await;
    let stamps = read_stamps(dir.path(), "lab").unwrap();
    assert_eq!(stamps.len(), 1);
    assert_eq!((stamps[0].record, stamps[0].user), (0, Some(user)));
}
