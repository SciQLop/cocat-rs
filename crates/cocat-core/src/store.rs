//! Append-only room files in the format the Python server already writes:
//! the `0.0.1\0` header, then `varuint(len) + update-v1` records.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use bytes::Bytes;
use thiserror::Error;

use crate::room_id::{InvalidRoomId, validate_room_id};

pub const HEADER: &[u8] = b"0.0.1\0";

const MAX_VARUINT_BYTES: usize = 10;

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

#[derive(Debug)]
pub struct FileRoomStore {
    path: PathBuf,
    file: File,
    updates: Vec<Bytes>,
    truncated_at: Option<u64>,
    // simplify: appended records wait here until an explicit flush; no Drop flush and
    // no background flusher. Phase 1 gets a durability policy with timed fsyncs.
    pending: Vec<u8>,
}

impl FileRoomStore {
    pub fn open(update_dir: &Path, room_id: &str) -> Result<Self, StoreError> {
        validate_room_id(room_id)?;
        let path = update_dir.join(format!("{room_id}.y"));
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
        if bytes.is_empty() {
            write_header(&mut file, &path)?;
        }
        if let Some(offset) = truncated_at {
            file.set_len(offset)
                .map_err(|source| io_error(&path, source))?;
        }

        Ok(Self {
            path,
            file,
            updates,
            truncated_at,
            pending: Vec::new(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn updates(&self) -> &[Bytes] {
        &self.updates
    }

    pub fn truncated_at(&self) -> Option<u64> {
        self.truncated_at
    }

    pub fn append(&mut self, update: Bytes) -> Result<(), StoreError> {
        write_varuint(update.len() as u64, &mut self.pending);
        self.pending.extend_from_slice(&update);
        self.updates.push(update);
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), StoreError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        self.file
            .write_all(&self.pending)
            .map_err(|source| io_error(&self.path, source))?;
        self.file
            .sync_data()
            .map_err(|source| io_error(&self.path, source))?;
        self.pending.clear();
        Ok(())
    }
}

fn append_handle(path: &Path) -> Result<File, StoreError> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| io_error(path, source))
}

fn write_header(file: &mut File, path: &Path) -> Result<(), StoreError> {
    file.write_all(HEADER)
        .map_err(|source| io_error(path, source))?;
    file.sync_data().map_err(|source| io_error(path, source))
}

fn parse_records(path: &Path, bytes: &[u8]) -> Result<(Vec<Bytes>, Option<u64>), StoreError> {
    if bytes.is_empty() {
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
