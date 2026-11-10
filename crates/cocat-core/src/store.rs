//! Append-only room files in the format the Python server already writes:
//! the `0.0.1\0` header, then `varuint(len) + update-v1` records.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use thiserror::Error;

use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};

use crate::room_id::InvalidRoomId;

pub const HEADER: &[u8] = b"0.0.1\0";

const MAX_VARUINT_BYTES: usize = 10;

static SYNC_COUNT: AtomicU64 = AtomicU64::new(0);

/// How many `sync` calls returned successfully in this process; the room timer is the only
/// caller, so tests can observe it without waiting on the filesystem.
pub fn sync_count() -> u64 {
    SYNC_COUNT.load(Ordering::Relaxed)
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error(transparent)]
    InvalidRoomId(#[from] InvalidRoomId),
    #[error("room file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("room file {path} does not start with the 0.0.1 header")]
    BadHeader { path: PathBuf },
}

/// The open file plus what was already in it. `open` hands the records over once instead
/// of retaining them: a room of 100k events must not be held twice in memory.
#[derive(Debug)]
pub struct FileRoomStore {
    path: PathBuf,
    file: File,
    truncated_at: Option<u64>,
    /// Appended records wait here until `write`; the caller owns when they reach the disk.
    pending: Vec<u8>,
    needs_sync: bool,
    records: u64,
}

impl FileRoomStore {
    pub fn open(update_dir: &Path, room_id: &str) -> Result<(Self, Vec<Bytes>), StoreError> {
        check_room_id(room_id)?;
        let path = room_file(update_dir, room_id);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(source) => return Err(io_error(&path, source)),
        };
        let (updates, truncated_at) = parse_records(&path, &bytes)?;
        if let Some(offset) = truncated_at {
            tracing::warn!(
                path = %path.display(),
                offset,
                "room file ends mid-record, rewriting from the last good offset"
            );
        }

        let mut file = append_handle(&path)?;
        if HEADER.starts_with(&bytes) {
            reset_to_header(&mut file, &path)?;
        } else if let Some(offset) = truncated_at {
            file.set_len(offset)
                .map_err(|source| io_error(&path, source))?;
        }

        let store = Self {
            path,
            file,
            truncated_at,
            pending: Vec::new(),
            needs_sync: false,
            records: updates.len() as u64,
        };
        Ok((store, updates))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn truncated_at(&self) -> Option<u64> {
        self.truncated_at
    }

    /// The room file's size on disk, which the server's memory budget turns into an
    /// estimate of what the loaded `Doc` costs.
    pub fn size(&self) -> u64 {
        self.file.metadata().map_or(0, |metadata| metadata.len())
    }

    /// The file's size once the pending records are written.
    pub fn total_size(&self) -> u64 {
        self.size().saturating_add(self.pending.len() as u64)
    }

    /// Records loaded at open plus records appended since, so the last append's index is
    /// `record_count() - 1`.
    pub fn record_count(&self) -> u64 {
        self.records
    }

    pub fn append(&mut self, update: Bytes) -> Result<(), StoreError> {
        write_varuint(update.len() as u64, &mut self.pending);
        self.pending.extend_from_slice(&update);
        self.records += 1;
        Ok(())
    }

    /// Writes the pending records, without waiting for the disk. A failed write is rolled
    /// back and the records stay pending for a retry.
    pub fn write(&mut self) -> Result<(), StoreError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let start = self
            .file
            .metadata()
            .map_err(|source| io_error(&self.path, source))?
            .len();
        if let Err(source) = self.file.write_all(&self.pending) {
            // Half a record defeats tail recovery, so cut it back and keep the records
            // in `pending` for a retry.
            if let Err(error) = self.file.set_len(start) {
                tracing::error!(
                    path = %self.path.display(),
                    %error,
                    "cannot roll a partial write back, the next open must recover the tail"
                );
            }
            return Err(io_error(&self.path, source));
        }
        self.pending.clear();
        self.needs_sync = true;
        Ok(())
    }

    /// Waits for the records written so far to reach the disk.
    pub fn sync(&mut self) -> Result<(), StoreError> {
        self.file
            .sync_data()
            .map_err(|source| io_error(&self.path, source))?;
        self.needs_sync = false;
        SYNC_COUNT.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub fn needs_sync(&self) -> bool {
        self.needs_sync
    }

    /// Empties the file back to its header, dropping every record, pending ones too.
    pub fn clear(&mut self) -> Result<(), StoreError> {
        self.pending.clear();
        reset_to_header(&mut self.file, &self.path)?;
        self.records = 0;
        self.truncated_at = None;
        self.needs_sync = false;
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), StoreError> {
        self.write()?;
        self.sync()
    }
}

/// What a file name cannot hold on Linux, macOS or Windows, plus `%` so that decoding is exact.
/// Python cocat named files after the raw room id (`Bepi MSA.y`, `alexis.jeandet.y`), so other
/// ASCII characters stay as they are and those files keep their names. Non-ASCII is escaped
/// (the crate's rule), which also avoids macOS normalising the name.
const NOT_IN_A_FILE_NAME: &AsciiSet = &CONTROLS
    .add(b'/')
    .add(b'\\')
    .add(b':')
    .add(b'*')
    .add(b'?')
    .add(b'"')
    .add(b'<')
    .add(b'>')
    .add(b'|')
    .add(b'%');

/// Most file systems cap a name at 255 bytes.
const MAX_FILE_NAME: usize = 255;

/// The file a room's records live in. Any room id maps to one file directly in `update_dir`:
/// a leading dot is escaped too, so `.` and `..` never name a directory.
// simplify: ids differing only by case share a file on case-insensitive file systems; the
// server's own ids are unique case-insensitively in practice, add a hash suffix if that changes.
pub fn room_file(update_dir: &Path, room_id: &str) -> PathBuf {
    update_dir.join(file_name(room_id))
}

/// The room id a file written by `room_file` belongs to, `None` for any other file.
pub fn room_id_from_file(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".y")?;
    let room_id = percent_decode_str(stem).decode_utf8().ok()?.into_owned();
    (file_name(&room_id) == name).then_some(room_id)
}

/// The room id as one file-system-safe name, with no extension: what `room_file` and a
/// room's folder of side files are named after.
pub fn escaped_room_id(room_id: &str) -> String {
    let escaped = utf8_percent_encode(room_id, NOT_IN_A_FILE_NAME).to_string();
    match escaped.strip_prefix('.') {
        Some(rest) => format!("%2E{rest}"),
        None => escaped,
    }
}

fn file_name(room_id: &str) -> String {
    format!("{}.y", escaped_room_id(room_id))
}

fn check_room_id(room_id: &str) -> Result<(), InvalidRoomId> {
    if room_id.is_empty() || file_name(room_id).len() > MAX_FILE_NAME {
        return Err(InvalidRoomId::new(room_id));
    }
    Ok(())
}

/// Every complete record in a room's file, read without writing: a torn tail is left for the
/// next writer to truncate, and a room that was never written reads as empty.
pub fn read_updates(update_dir: &Path, room_id: &str) -> Result<Vec<Bytes>, StoreError> {
    check_room_id(room_id)?;
    let path = room_file(update_dir, room_id);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(io_error(&path, source)),
    };
    let (updates, _) = parse_records(&path, &bytes)?;
    Ok(updates)
}

fn append_handle(path: &Path) -> Result<File, StoreError> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| io_error(path, source))
}

fn reset_to_header(file: &mut File, path: &Path) -> Result<(), StoreError> {
    file.set_len(0).map_err(|source| io_error(path, source))?;
    file.write_all(HEADER)
        .map_err(|source| io_error(path, source))?;
    file.sync_data().map_err(|source| io_error(path, source))
}

fn parse_records(path: &Path, bytes: &[u8]) -> Result<(Vec<Bytes>, Option<u64>), StoreError> {
    if HEADER.starts_with(bytes) {
        return Ok((Vec::new(), None));
    }
    let body = bytes
        .strip_prefix(HEADER)
        .ok_or_else(|| StoreError::BadHeader {
            path: path.to_owned(),
        })?;

    let mut updates = Vec::new();
    let mut offset = HEADER.len();
    let mut input = body;
    while let Some(len) = read_varuint(&mut input) {
        // Python never writes an empty record; a zero length is a cut tail.
        if len == 0 {
            break;
        }
        let Ok(len) = usize::try_from(len) else {
            break;
        };
        let Some(record) = input.get(..len) else {
            break;
        };
        updates.push(Bytes::copy_from_slice(record));
        input = &input[len..];
        offset = bytes.len() - input.len();
    }

    let truncated_at = (offset < bytes.len()).then_some(offset as u64);
    Ok((updates, truncated_at))
}

fn io_error(path: &Path, source: io::Error) -> StoreError {
    StoreError::Io {
        path: path.to_owned(),
        source,
    }
}

/// Reads a base-128 varuint, little-endian groups, as lib0 writes it.
fn read_varuint(input: &mut &[u8]) -> Option<u64> {
    let mut value = 0u64;
    for shift in 0..MAX_VARUINT_BYTES {
        let (byte, rest) = input.split_first()?;
        *input = rest;
        value |= u64::from(byte & 0x7f) << (shift * 7);
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn write_varuint(mut value: u64, out: &mut Vec<u8>) {
    while value > 0x7f {
        out.push(0x80 | (value & 0x7f) as u8);
        value >>= 7;
    }
    out.push(value as u8);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_reaches_the_file_without_a_sync() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _) = FileRoomStore::open(dir.path(), "room").unwrap();
        let record = Bytes::from_static(b"update-bytes");
        store.append(record.clone()).unwrap();

        store.write().unwrap();
        assert!(store.needs_sync());
        let (_, updates) = FileRoomStore::open(dir.path(), "room").unwrap();
        assert_eq!(updates, [record]);
    }

    #[test]
    fn a_cleared_store_reopens_empty_and_takes_new_records() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _) = FileRoomStore::open(dir.path(), "room").unwrap();
        store.append(Bytes::from_static(b"old")).unwrap();
        store.write().unwrap();
        store.append(Bytes::from_static(b"pending")).unwrap();

        store.clear().unwrap();
        let record = Bytes::from_static(b"new");
        store.append(record.clone()).unwrap();
        store.write().unwrap();

        assert_eq!(store.record_count(), 1);
        let (_, updates) = FileRoomStore::open(dir.path(), "room").unwrap();
        assert_eq!(updates, [record]);
    }

    #[test]
    fn a_sync_clears_the_dirty_flag() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _) = FileRoomStore::open(dir.path(), "room").unwrap();
        store.append(Bytes::from_static(b"update-bytes")).unwrap();
        store.write().unwrap();
        let before = sync_count();

        store.sync().unwrap();
        assert!(sync_count() > before);
        assert!(!store.needs_sync());
    }

    // A unit test because this failure needs a file handle the public API never opens.
    #[test]
    fn a_failed_flush_keeps_the_records_for_a_retry() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _) = FileRoomStore::open(dir.path(), "room").unwrap();
        let record = Bytes::from_static(b"update-bytes");
        store.append(record.clone()).unwrap();
        let path = store.path().to_owned();

        store.file = File::open(&path).unwrap();
        let error = store.flush().unwrap_err();
        assert!(matches!(error, StoreError::Io { .. }));
        assert_eq!(fs::read(&path).unwrap(), HEADER);

        store.file = append_handle(&path).unwrap();
        store.flush().unwrap();
        let (_, updates) = FileRoomStore::open(dir.path(), "room").unwrap();
        assert_eq!(updates, [record]);
    }

    #[test]
    fn the_record_count_spans_loaded_and_appended_records() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _) = FileRoomStore::open(dir.path(), "room").unwrap();
        store.append(Bytes::from_static(b"first")).unwrap();
        store.flush().unwrap();

        let (mut store, _) = FileRoomStore::open(dir.path(), "room").unwrap();
        assert_eq!(store.record_count(), 1);
        store.append(Bytes::from_static(b"second")).unwrap();
        assert_eq!(store.record_count(), 2);
    }

    #[test]
    fn a_stamps_file_is_not_a_room_file() {
        assert_eq!(room_id_from_file(Path::new("lab.y.h")), None);
        assert_eq!(
            room_id_from_file(Path::new("lab.y")),
            Some("lab".to_owned())
        );
    }

    #[test]
    fn read_updates_leaves_a_torn_tail() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _) = FileRoomStore::open(dir.path(), "room").unwrap();
        store.append(Bytes::from_static(b"first")).unwrap();
        store.append(Bytes::from_static(b"second")).unwrap();
        store.write().unwrap();
        let path = store.path().to_owned();
        let complete = fs::metadata(&path).unwrap().len();
        // A length prefix that promises more bytes than the file holds.
        let mut torn = OpenOptions::new().append(true).open(&path).unwrap();
        torn.write_all(&[0x40, b't', b'r']).unwrap();

        let updates = read_updates(dir.path(), "room").unwrap();

        assert_eq!(
            updates,
            [Bytes::from_static(b"first"), Bytes::from_static(b"second")]
        );
        assert_eq!(
            fs::metadata(&path).unwrap().len(),
            complete + 3,
            "the read leaves the torn tail for the next writer to truncate"
        );
    }
}
