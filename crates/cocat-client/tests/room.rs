mod support;

use cocat_client::Room;
use cocat_core::store;
use support::*;
use tokio::sync::broadcast::error::TryRecvError;
use yrs::Transact;

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

/// A room Python cocat wrote, in layout v1.
const V1_ROOM: &[u8] = include_bytes!("../../cocat-core/tests/fixtures/catalogues.y");

#[test]
fn a_cache_of_the_first_layout_is_discarded_on_open() {
    let dir = cache();
    std::fs::write(store::room_file(dir.path(), ROOM), V1_ROOM).unwrap();

    let room = Room::open(dir.path(), ROOM).unwrap();
    assert!(names(&room).is_empty());
    room.close();

    assert!(store::read_updates(dir.path(), ROOM).unwrap().is_empty());
}

#[test]
fn a_discarded_cache_keeps_what_is_written_after() {
    let dir = cache();
    std::fs::write(store::room_file(dir.path(), ROOM), V1_ROOM).unwrap();
    {
        let room = Room::open(dir.path(), ROOM).unwrap();
        create_catalogue(&room, CATALOGUE, "alpha");
        room.close();
    }

    let room = Room::open(dir.path(), ROOM).unwrap();
    assert_eq!(names(&room), ["alpha"]);
}

#[test]
fn an_abandoned_batch_leaves_the_room_as_it_was() {
    const KEPT: &str = "11111111-2222-4333-8444-555555555555";
    const MADE: &str = "22222222-3333-4444-8555-666666666666";
    let dir = cache();
    let room = Room::open(dir.path(), ROOM).unwrap();
    create_catalogue(&room, CATALOGUE, "alpha");
    create_event(&room, KEPT, "2025-01-31 10:00:00", "2025-01-31 10:01:00");

    room.begin_batch();
    create_event(&room, MADE, "2025-02-01 10:00:00", "2025-02-01 10:01:00");
    room.write(|txn| {
        cocat_core::model::rename_catalogue(txn, CATALOGUE, "beta");
        cocat_core::model::purge_event(txn, KEPT);
    })
    .unwrap();
    room.abandon_batch();

    assert_eq!(names(&room), ["alpha"]);
    assert!(holds(&room, KEPT), "the purged event is back");
    assert!(!holds(&room, MADE), "the new event is gone");
    room.close();

    let reopened = Room::open(dir.path(), ROOM).unwrap();
    assert_eq!(names(&reopened), ["alpha"]);
    assert!(holds(&reopened, KEPT) && !holds(&reopened, MADE));
}

fn holds(room: &Room, event: &str) -> bool {
    cocat_core::model::event(&room.doc().transact(), event).is_some()
}

/// SciQLop, 2026-11-26: ending a batch while another thread read the document panicked
/// (ExclusiveAcqFailed, a BaseException Python cannot catch): the batch's undo manager was
/// dropped, and its drop needs the document to itself, without waiting.
#[test]
fn a_batch_ends_while_another_thread_reads_the_document() {
    let dir = cache();
    let room = Room::open(dir.path(), ROOM).unwrap();
    for round in 0..3 {
        room.begin_batch();
        create_catalogue(
            &room,
            &format!("6f9c1a52-0b0d-4f4e-9d5e-1b2c3d4e5f6{round}"),
            "alpha",
        );
        let doc = room.doc();
        let (held, release) = (std::sync::Barrier::new(2), std::sync::Barrier::new(2));
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _reading = doc.transact();
                held.wait();
                release.wait();
            });
            held.wait();
            let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| room.end_batch()));
            release.wait();
            assert!(ended.is_ok(), "end_batch panicked in round {round}");
        });
    }
    room.begin_batch();
    create_catalogue(&room, CATALOGUE, "beta");
    room.abandon_batch();
    assert_eq!(names(&room).len(), 3, "the abandoned batch is taken back");
    room.close();
}
