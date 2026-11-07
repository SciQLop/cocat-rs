use bytes::Bytes;
use cocat_core::store::{FileRoomStore, StoreError, room_file, room_id_from_file};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use yrs::updates::decoder::Decode;
use yrs::{Doc, Map, Transact, Update};

const FIXTURE: &[u8] = include_bytes!("fixtures/python_two_events.y");
const GOOD_RECORDS_END: u64 = 70;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn write_room(dir: &Path, room_id: &str, bytes: &[u8]) {
    fs::write(dir.join(format!("{room_id}.y")), bytes).unwrap();
}

#[test]
fn reads_a_file_written_by_pycrdt() {
    let (store, updates) = FileRoomStore::open(&fixture_dir(), "python_two_events").unwrap();
    assert_eq!(updates.len(), 2);
    assert_eq!(store.truncated_at(), None);

    let doc = Doc::new();
    for update in &updates {
        let update = Update::decode_v1(update).unwrap();
        doc.transact_mut().apply_update(update).unwrap();
    }
    let content = doc.get_or_insert_map("content");
    let txn = doc.transact();
    assert_eq!(content.get(&txn, "key").unwrap().to_string(&txn), "value");
}

#[test]
fn stops_at_a_truncated_record() {
    let dir = TempDir::new().unwrap();
    let mut bytes = Bytes::from_static(FIXTURE).to_vec();
    bytes.extend_from_slice(&[0x1f, 0x01, 0x01]);

    write_room(dir.path(), "cut", &bytes);
    let (store, updates) = FileRoomStore::open(dir.path(), "cut").unwrap();

    assert_eq!(updates.len(), 2);
    assert_eq!(store.truncated_at(), Some(GOOD_RECORDS_END));
}

#[test]
fn a_zero_length_record_is_a_truncated_tail() {
    let dir = TempDir::new().unwrap();
    let mut bytes = Bytes::from_static(FIXTURE).to_vec();
    bytes.push(0);

    write_room(dir.path(), "zero", &bytes);
    let (store, updates) = FileRoomStore::open(dir.path(), "zero").unwrap();

    assert_eq!(updates.len(), 2);
    assert_eq!(store.truncated_at(), Some(GOOD_RECORDS_END));
}

#[test]
fn a_partial_header_is_treated_like_an_empty_file() {
    let dir = TempDir::new().unwrap();
    write_room(dir.path(), "partial", b"0.0");

    let (_, updates) = FileRoomStore::open(dir.path(), "partial").unwrap();
    assert!(updates.is_empty());
    assert_eq!(fs::read(dir.path().join("partial.y")).unwrap(), b"0.0.1\0");
}

#[test]
fn rejects_a_file_without_the_version_header() {
    let dir = TempDir::new().unwrap();
    write_room(dir.path(), "nope", b"nope");

    let error = FileRoomStore::open(dir.path(), "nope").unwrap_err();
    assert!(matches!(error, StoreError::BadHeader { .. }));
}

#[test]
fn a_missing_file_opens_as_an_empty_room() {
    let dir = TempDir::new().unwrap();

    let (store, updates) = FileRoomStore::open(dir.path(), "absent").unwrap();
    assert!(updates.is_empty());
    assert_eq!(store.truncated_at(), None);
}

#[test]
fn rejects_an_id_without_a_usable_file_name() {
    let dir = TempDir::new().unwrap();

    for id in ["", "x".repeat(300).as_str()] {
        let error = FileRoomStore::open(dir.path(), id).unwrap_err();
        assert!(matches!(error, StoreError::InvalidRoomId(_)), "id {id:?}");
    }
}

#[test]
fn python_room_ids_keep_their_file_names() {
    let dir = Path::new("rooms");

    for id in ["room0", "alexis.jeandet", "Bepi MSA", "CDPP Tests"] {
        assert_eq!(room_file(dir, id), dir.join(format!("{id}.y")));
    }
}

#[test]
fn any_room_id_gets_its_own_file_inside_the_directory() {
    let dir = TempDir::new().unwrap();
    let ids = [
        "Bepi MSA",
        ".",
        "..",
        "../escape",
        "a/b",
        "a\\b",
        "C:x",
        "50%",
        "a%2Fb",
        "Équipe",
        ".hidden",
    ];

    for id in ids {
        let (mut store, _) = FileRoomStore::open(dir.path(), id).unwrap();
        store.append(Bytes::from_static(b"\x00\x00")).unwrap();
        store.flush().unwrap();
        assert_eq!(store.path().parent(), Some(dir.path()), "id {id:?}");
        assert_eq!(room_id_from_file(store.path()).as_deref(), Some(id));
    }
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), ids.len());
}
