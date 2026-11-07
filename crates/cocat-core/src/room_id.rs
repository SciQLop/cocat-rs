//! The rule for the ids of rooms created on this server. Ids imported from Python cocat may
//! break it (spaces, dots); `store::room_file` turns any id into a safe file name.

use thiserror::Error;

/// New room ids are `[A-Za-z0-9_-]{1,64}`: plain in URLs and file names.
pub const MAX_ROOM_ID_LEN: usize = 64;

#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid room id: {0:?}")]
pub struct InvalidRoomId(String);

impl InvalidRoomId {
    pub fn new(id: &str) -> Self {
        Self(id.to_owned())
    }
}

pub fn validate_room_id(id: &str) -> Result<(), InvalidRoomId> {
    let safe = !id.is_empty()
        && id.len() <= MAX_ROOM_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if safe {
        Ok(())
    } else {
        Err(InvalidRoomId::new(id))
    }
}
