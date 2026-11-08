//! Who wrote each room record and when, in a side file beside the room file (`<room>.y.h`),
//! so the room file format stays the one Python reads. One fixed-size entry per record.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use bytes::Bytes;
use cocat_core::store::room_file;
use uuid::Uuid;
use xxhash_rust::xxh3::xxh3_64;

/// record u64, user 16 bytes (all zero = none), at_ms i64, len u32, restore u32, hash u64;
/// all little-endian. `restore` is 0 for an ordinary record, else the restored version + 1,
/// so entries written before restores existed read as ordinary.
pub const ENTRY: usize = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub record: u64,
    pub user: Option<Uuid>,
    pub at_ms: i64,
    pub len: u32,
    pub hash: u64,
    /// The version this record restored, when it is a restore.
    pub restored_from: Option<u64>,
}

pub fn stamp_for(record: u64, user: Option<Uuid>, at_ms: i64, bytes: &[u8]) -> Stamp {
    Stamp {
        record,
        user,
        at_ms,
        len: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
        hash: xxh3_64(bytes),
        restored_from: None,
    }
}

impl Stamp {
    fn encode(&self) -> [u8; ENTRY] {
        let mut entry = [0u8; ENTRY];
        entry[0..8].copy_from_slice(&self.record.to_le_bytes());
        entry[8..24].copy_from_slice(self.user.unwrap_or(Uuid::nil()).as_bytes());
        entry[24..32].copy_from_slice(&self.at_ms.to_le_bytes());
        entry[32..36].copy_from_slice(&self.len.to_le_bytes());
        entry[36..40].copy_from_slice(&encode_restore(self.restored_from).to_le_bytes());
        entry[40..48].copy_from_slice(&self.hash.to_le_bytes());
        entry
    }

    fn decode(entry: &[u8]) -> Option<Self> {
        let (record, rest) = entry.split_first_chunk::<8>()?;
        let (user, rest) = rest.split_first_chunk::<16>()?;
        let (at_ms, rest) = rest.split_first_chunk::<8>()?;
        let (len, rest) = rest.split_first_chunk::<4>()?;
        let (restore, rest) = rest.split_first_chunk::<4>()?;
        let (hash, _) = rest.split_first_chunk::<8>()?;
        Some(Self {
            record: u64::from_le_bytes(*record),
            user: Some(Uuid::from_bytes(*user)).filter(|user| !user.is_nil()),
            at_ms: i64::from_le_bytes(*at_ms),
            len: u32::from_le_bytes(*len),
            hash: u64::from_le_bytes(*hash),
            restored_from: u32::from_le_bytes(*restore).checked_sub(1).map(u64::from),
        })
    }

    fn matches(&self, record: &[u8]) -> bool {
        usize::try_from(self.len).is_ok_and(|len| len == record.len())
            && self.hash == xxh3_64(record)
    }
}

/// simplify: a version id past `u32::MAX - 1` (four billion records in one room, far
/// beyond any storage limit) loses the restore mark and groups as an ordinary edit; grow the
/// entry to 56 bytes behind a format marker if rooms ever get there.
fn encode_restore(restored_from: Option<u64>) -> u32 {
    restored_from
        .and_then(|version| u32::try_from(version).ok())
        .and_then(|version| version.checked_add(1))
        .unwrap_or(0)
}

pub fn stamps_file(update_dir: &Path, room_id: &str) -> PathBuf {
    let mut path = room_file(update_dir, room_id).into_os_string();
    path.push(".h");
    PathBuf::from(path)
}

/// The open stamps file. Like `FileRoomStore`, appended stamps wait in memory until `write`,
/// and `sync` waits for the disk; the room thread calls both on the store's cadence.
#[derive(Debug)]
pub struct StampFile {
    file: File,
    pending: Vec<u8>,
    needs_sync: bool,
    /// The file's length once the pending stamps are written; it counts in the room's size.
    size: u64,
}

impl StampFile {
    /// Cuts a torn last entry, so the entries appended after it stay aligned.
    pub fn open(update_dir: &Path, room_id: &str) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(stamps_file(update_dir, room_id))?;
        let len = file.metadata()?.len();
        let whole = len - len % ENTRY as u64;
        if whole != len {
            file.set_len(whole)?;
        }
        Ok(Self {
            file,
            pending: Vec::new(),
            needs_sync: false,
            size: whole,
        })
    }

    pub fn append(&mut self, stamp: Stamp) {
        self.pending.extend_from_slice(&stamp.encode());
        self.size += ENTRY as u64;
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// A failed write is cut back to the last whole entry and the stamps stay pending.
    pub fn write(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let start = self.file.metadata()?.len();
        if let Err(error) = self.file.write_all(&self.pending) {
            let _ = self.file.set_len(start);
            return Err(error);
        }
        self.pending.clear();
        self.needs_sync = true;
        Ok(())
    }

    pub fn sync(&mut self) -> io::Result<()> {
        if self.needs_sync {
            self.file.sync_data()?;
            self.needs_sync = false;
        }
        Ok(())
    }
}

/// Every whole entry of the room's stamps file; a missing file has none.
pub fn read_stamps(update_dir: &Path, room_id: &str) -> io::Result<Vec<Stamp>> {
    let bytes = match fs::read(stamps_file(update_dir, room_id)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let (entries, _torn) = bytes.as_chunks::<ENTRY>();
    Ok(entries
        .iter()
        .filter_map(|entry| Stamp::decode(entry))
        .collect())
}

/// One slot per record: the stamp written for it, or `None` when it has none that still
/// matches its bytes (written before history, or the room file was replaced since).
pub fn valid_stamps(records: &[Bytes], stamps: Vec<Stamp>) -> Vec<Option<Stamp>> {
    let mut slots = vec![None; records.len()];
    for stamp in stamps {
        let Ok(index) = usize::try_from(stamp.record) else {
            continue;
        };
        if let (Some(slot), Some(record)) = (slots.get_mut(index), records.get(index))
            && stamp.matches(record)
        {
            *slot = Some(stamp);
        }
    }
    slots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stamp_round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let written = [
            stamp_for(0, Some(Uuid::new_v4()), 1_700_000_000_000, b"first"),
            stamp_for(1, None, -5, b"second"),
        ];
        let mut stamps = StampFile::open(dir.path(), "lab").unwrap();
        for stamp in written {
            stamps.append(stamp);
        }
        stamps.write().unwrap();
        stamps.sync().unwrap();

        assert_eq!(read_stamps(dir.path(), "lab").unwrap(), written);
    }

    #[test]
    fn a_restore_stamp_names_the_version_it_restored() {
        let dir = tempfile::tempdir().unwrap();
        let restore = Stamp {
            restored_from: Some(7),
            ..stamp_for(3, Some(Uuid::new_v4()), 1, b"restore")
        };
        let mut stamps = StampFile::open(dir.path(), "lab").unwrap();
        stamps.append(restore);
        stamps.write().unwrap();

        assert_eq!(read_stamps(dir.path(), "lab").unwrap(), [restore]);
    }

    #[test]
    fn an_entry_with_zero_spare_bytes_is_not_a_restore() {
        let stamp = stamp_for(0, None, 1, b"first");
        let mut entry = stamp.encode();
        entry[36..40].fill(0);

        assert_eq!(Stamp::decode(&entry).unwrap().restored_from, None);
    }

    #[test]
    fn a_torn_last_entry_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let stamp = stamp_for(0, None, 1, b"first");
        let mut stamps = StampFile::open(dir.path(), "lab").unwrap();
        stamps.append(stamp);
        stamps.write().unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(stamps_file(dir.path(), "lab"))
            .unwrap();
        file.write_all(&[0xAB; 20]).unwrap();

        assert_eq!(read_stamps(dir.path(), "lab").unwrap(), [stamp]);
    }

    #[test]
    fn reopening_cuts_a_torn_entry_so_later_stamps_stay_aligned() {
        let dir = tempfile::tempdir().unwrap();
        let first = stamp_for(0, None, 1, b"first");
        let second = stamp_for(1, None, 2, b"second");
        let mut stamps = StampFile::open(dir.path(), "lab").unwrap();
        stamps.append(first);
        stamps.write().unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(stamps_file(dir.path(), "lab"))
            .unwrap();
        file.write_all(&[0xAB; 20]).unwrap();

        let mut stamps = StampFile::open(dir.path(), "lab").unwrap();
        stamps.append(second);
        stamps.write().unwrap();

        assert_eq!(read_stamps(dir.path(), "lab").unwrap(), [first, second]);
    }

    #[test]
    fn stamps_that_do_not_match_their_record_are_dropped() {
        let records = [
            Bytes::from_static(b"a"),
            Bytes::from_static(b"b"),
            Bytes::from_static(b"c"),
        ];
        let good = stamp_for(0, None, 1, b"a");
        let stamps = vec![
            good,
            stamp_for(2, None, 2, b"x"),
            stamp_for(7, None, 3, b"c"),
        ];

        assert_eq!(valid_stamps(&records, stamps), [Some(good), None, None]);
    }

    #[test]
    fn the_stamps_file_sits_beside_the_room_file() {
        let dir = Path::new("rooms");
        assert_eq!(stamps_file(dir, "lab"), dir.join("lab.y.h"));
    }
}
