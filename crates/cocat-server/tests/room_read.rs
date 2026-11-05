use std::fs;
use std::time::Duration;

use bytes::Bytes;
use cocat_core::role::Role;
use cocat_server::room::{MEMORY_PER_FILE_BYTE, RoomError, RoomManager, outbox};
use tempfile::TempDir;
use tokio::time::timeout;
use uuid::Uuid;
use yrs::sync::{Message, SyncMessage};
use yrs::updates::encoder::Encode;
use yrs::{Doc, Map, ReadTxn, StateVector, Transact};

const WAIT: Duration = Duration::from_secs(10);
const ROOM: &str = "room";

fn manager(dir: &TempDir, budget: Option<u64>) -> RoomManager {
    RoomManager::new(dir.path().to_owned(), budget)
}

fn update_frame(key: &str, value: &str) -> Bytes {
    let doc = Doc::new();
    doc.get_or_insert_map("content")
        .insert(&mut doc.transact_mut(), key, value);
    let update = doc
        .transact()
        .encode_state_as_update_v1(&StateVector::default());
    Bytes::from(Message::Sync(SyncMessage::Update(update)).encode_v1())
}

fn content(doc: &Doc, key: &str) -> Option<String> {
    let txn = doc.transact();
    let map = txn.get_map("content")?;
    map.get(&txn, key).map(|value| value.to_string(&txn))
}

fn size(dir: &TempDir, room_id: &str) -> u64 {
    fs::metadata(dir.path().join(format!("{room_id}.y")))
        .unwrap()
        .len()
}

/// Attaches a client, applies one update, and lets the last detach unload the room so the
/// records reach the file.
async fn write_room(manager: &RoomManager, room_id: &str, key: &str, value: &str) {
    let handle = manager.get_or_open(room_id).unwrap();
    let (out, _inbox) = outbox();
    handle
        .attach(1, Uuid::new_v4(), Role::Editor, out)
        .expect("the room accepted the client");
    handle.frame(1, update_frame(key, value));
    handle.detach(1).await;
}

#[tokio::test]
async fn reads_an_open_room_through_its_thread() {
    let dir = TempDir::new().unwrap();
    let manager = manager(&dir, None);
    let handle = manager.get_or_open(ROOM).unwrap();
    let (out, _inbox) = outbox();
    handle.attach(1, Uuid::new_v4(), Role::Editor, out).unwrap();
    handle.frame(1, update_frame("key", "value"));

    let value = manager.read(ROOM, |doc| content(doc, "key")).await.unwrap();

    assert_eq!(value.as_deref(), Some("value"));
    assert_eq!(manager.open_rooms(), 1, "the room was open all along");
}

#[tokio::test]
async fn reads_a_closed_room_from_its_file() {
    let dir = TempDir::new().unwrap();
    let manager = manager(&dir, None);
    write_room(&manager, ROOM, "key", "value").await;
    assert_eq!(manager.open_rooms(), 0, "the room unloaded with its client");

    let value = manager.read(ROOM, |doc| content(doc, "key")).await.unwrap();

    assert_eq!(value.as_deref(), Some("value"));
    assert_eq!(
        manager.open_rooms(),
        0,
        "a read does not keep the room open"
    );
}

#[tokio::test]
async fn missing_file_reads_an_empty_doc() {
    let dir = TempDir::new().unwrap();
    let manager = manager(&dir, None);

    let has_catalogues = manager
        .read("nothing", |doc| {
            doc.transact().get_map("catalogues").is_some()
        })
        .await
        .unwrap();

    assert!(!has_catalogues);
    assert_eq!(manager.open_rooms(), 0);
}

#[tokio::test]
async fn cold_read_over_budget_is_refused() {
    let dir = TempDir::new().unwrap();
    let generous = manager(&dir, None);
    write_room(&generous, ROOM, "key", "value").await;

    let tight = manager(&dir, Some(1));
    let refused = tight.read(ROOM, |doc| content(doc, "key")).await;

    assert!(
        matches!(refused, Err(RoomError::OverBudget { .. })),
        "a cold read past the budget is refused, got {refused:?}"
    );
}

#[tokio::test]
async fn cold_read_releases_its_budget() {
    let dir = TempDir::new().unwrap();
    let generous = manager(&dir, None);
    write_room(&generous, "first", "key", &"x".repeat(200)).await;
    write_room(&generous, "second", "key", &"y".repeat(200)).await;
    let estimate = size(&dir, "first").max(size(&dir, "second")) * MEMORY_PER_FILE_BYTE;

    let tight = manager(&dir, Some(estimate + 1));
    tight
        .read("first", |doc| content(doc, "key"))
        .await
        .unwrap();

    tight
        .get_or_open("second")
        .expect("the cold read released its share of the budget");
}

#[tokio::test]
async fn read_survives_an_unload_race() {
    let dir = TempDir::new().unwrap();
    let manager = manager(&dir, None);
    write_room(&manager, ROOM, "key", "value").await;

    for _ in 0..100 {
        let handle = loop {
            let handle = manager.get_or_open(ROOM).unwrap();
            let (out, _inbox) = outbox();
            if handle.attach(1, Uuid::new_v4(), Role::Editor, out).is_ok() {
                break handle;
            }
        };
        let reading = manager.read(ROOM, |doc| content(doc, "key"));
        let (answered, ()) = tokio::join!(timeout(WAIT, reading), handle.detach(1));
        let value = answered
            .expect("a read during an unload answers")
            .expect("the read succeeds");

        assert_eq!(value.as_deref(), Some("value"));
    }
    assert_eq!(manager.open_rooms(), 0);
}
