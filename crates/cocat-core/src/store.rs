//! Append-only room files in the format the Python server already writes:
//! the `0.0.1\0` header, then `varuint(len) + update-v1` records.

use std::fs;
use std::io;
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
    updates: Vec<Bytes>,
    truncated_at: Option<u64>,
}

impl FileRoomStore {
    pub fn open(update_dir: &Path, room_id: &str) -> Result<Self, StoreError> {
        validate_room_id(room_id)?;
        let path = update_dir.join(format!("{room_id}.y"));
        let file = match fs::read(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(source) => return Err(StoreError::Io { path, source }),
        };
        let (updates, truncated_at) = parse_records(&path, &file)?;
        if let Some(offset) = truncated_at {
            tracing::warn!(path = %path.display(), offset, "room file ends mid-record");
        }
        Ok(Self {
            path,
            updates,
            truncated_at,
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
