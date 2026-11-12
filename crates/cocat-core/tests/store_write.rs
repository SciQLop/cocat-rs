use bytes::Bytes;
use cocat_core::store::{FileRoomStore, HEADER};
use std::fs;
use tempfile::TempDir;

const FIXTURE: &[u8] = include_bytes!("fixtures/python_two_events.y");
const RECORD_LEN: usize = 31;

fn update_at(payload_start: usize) -> Bytes {
    Bytes::copy_from_slice(&FIXTURE[payload_start..payload_start + RECORD_LEN])
}

#[test]
fn open_creates_a_header_only_file() {
    let dir = TempDir::new().unwrap();

    let (store, updates) = FileRoomStore::open(dir.path(), "fresh").unwrap();

    assert_eq!(store.path(), dir.path().join("fresh.y"));
    assert_eq!(fs::read(store.path()).unwrap(), HEADER);
    assert!(updates.is_empty());
}

#[test]
fn append_writes_a_length_prefixed_record() {
    let dir = TempDir::new().unwrap();
    let update = update_at(7);

    let (mut store, _) = FileRoomStore::open(dir.path(), "room").unwrap();
    store.append(update.clone()).unwrap();
    store.flush().unwrap();

    let mut expected = HEADER.to_vec();
    expected.push(RECORD_LEN as u8);
    expected.extend_from_slice(&update);
    assert_eq!(fs::read(store.path()).unwrap(), expected);
}

#[test]
fn flush_survives_a_reopen() {
    let dir = TempDir::new().unwrap();
    let update = update_at(7);

    {
        let (mut store, _) = FileRoomStore::open(dir.path(), "room").unwrap();
        store.append(update.clone()).unwrap();
        store.flush().unwrap();
    }

    let (_, updates) = FileRoomStore::open(dir.path(), "room").unwrap();
    assert_eq!(updates, [update]);
}

#[test]
fn a_damaged_length_mid_file_keeps_the_bytes_it_cuts_beside_the_file() {
    let dir = TempDir::new().unwrap();
    let mut damaged = FIXTURE.to_vec();
    damaged[6] = 0x7f;
    fs::write(dir.path().join("bad.y"), &damaged).unwrap();

    let (store, updates) = FileRoomStore::open(dir.path(), "bad").unwrap();

    assert!(updates.is_empty());
    assert_eq!(store.truncated_at(), Some(6));
    let kept: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with("bad.y.cut-"))
        .collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(fs::read(dir.path().join(&kept[0])).unwrap(), &damaged[6..]);
}

#[test]
fn append_after_a_truncated_tail_rewrites_from_the_good_offset() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("cut.y"), &FIXTURE[..FIXTURE.len() - 3]).unwrap();

    let (mut store, _) = FileRoomStore::open(dir.path(), "cut").unwrap();
    assert_eq!(store.truncated_at(), Some(38));
    store.append(update_at(39)).unwrap();
    store.flush().unwrap();
    assert_eq!(fs::read(store.path()).unwrap(), FIXTURE);

    drop(store);
    let (store, updates) = FileRoomStore::open(dir.path(), "cut").unwrap();
    assert_eq!(updates.len(), 2);
    assert_eq!(store.truncated_at(), None);
}
