mod support;

use cocat_client::Room;
use cocat_core::store;
use support::*;
use tokio::sync::broadcast::error::TryRecvError;

const ROOM: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const CATALOGUE: &str = "6f9c1a52-0b0d-4f4e-9d5e-1b2c3d4e5f60";

#[test]
fn a_local_write_survives_a_reopen() {
    let dir = cache();
    {
        let room = Room::open(dir.path(), ROOM).unwrap();
        create_catalogue(&room, CATALOGUE, "alpha");
        room.close();
    }

    let room = Room::open(dir.path(), ROOM).unwrap();
    assert_eq!(names(&room), ["alpha"]);
}

#[test]
fn the_cache_file_holds_the_local_update() {
    let dir = cache();
    let room = Room::open(dir.path(), ROOM).unwrap();
    create_catalogue(&room, CATALOGUE, "alpha");
    room.close();

    let records = store::read_updates(dir.path(), ROOM).unwrap();
    assert_eq!(records.len(), 1, "one local transaction is one record");
}

#[test]
fn a_local_write_produces_no_changes() {
    let dir = cache();
    let room = Room::open(dir.path(), ROOM).unwrap();
    let mut changes = room.changes();

    create_catalogue(&room, CATALOGUE, "alpha");

    assert!(matches!(changes.try_recv(), Err(TryRecvError::Empty)));
}

#[test]
fn any_listed_room_id_opens_inside_the_cache() {
    let dir = cache();
    for id in ["Bepi MSA", "../escape", "a/b", ".."] {
        Room::open(dir.path(), id).unwrap().close();
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 4);
}

#[test]
fn opening_an_empty_directory_creates_the_cache() {
    let dir = cache();
    let room = Room::open(dir.path(), ROOM).unwrap();
    assert_eq!(room.status(), cocat_client::Status::Offline);
    assert!(names(&room).is_empty());
}
