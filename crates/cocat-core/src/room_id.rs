//! The rule for the ids of rooms created on this server. Ids imported from Python cocat may
//! break it (spaces, dots); `store::room_file` turns any id into a safe file name.

use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
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

/// What a URL path segment cannot hold as is: the `url` crate's (and browsers') set, plus
/// `\`, which browsers read as `/` in an http(s) path.
const NOT_IN_A_SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'/')
    .add(b'\\')
    .add(b'%');

/// An id as one URL path segment, for the server's links and the app's alike: an imported
/// id may hold spaces, dots, `?`, `#` or `%`.
pub fn path_segment(id: &str) -> String {
    utf8_percent_encode(id, NOT_IN_A_SEGMENT).to_string()
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

/// The ids of public catalogues start with this. A dot never appears in the ids this server
/// makes for projects, so only an id imported from Python could clash; the server refuses to
/// start on one rather than publish it.
pub const PUBLIC_PREFIX: &str = "public.";

pub fn is_public_id(id: &str) -> bool {
    id.starts_with(PUBLIC_PREFIX)
}

/// A public catalogue's id from its key: dot-separated parts of `[A-Za-z0-9_-]`, so the id
/// reads as a namespace (`public.amda.sharedcatalog_22`) and never names a parent directory.
pub fn public_id(key: &str) -> Result<String, InvalidRoomId> {
    let part = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    };
    match key.len() <= MAX_ROOM_ID_LEN && key.split('.').all(part) {
        true => Ok(format!("{PUBLIC_PREFIX}{key}")),
        false => Err(InvalidRoomId::new(key)),
    }
}

#[cfg(test)]
mod tests {
    use super::{is_public_id, path_segment, public_id};

    #[test]
    fn a_public_id_is_the_prefix_and_dotted_parts() {
        assert_eq!(
            public_id("amda.sharedcatalog_22").unwrap(),
            "public.amda.sharedcatalog_22"
        );
        assert!(is_public_id("public.amda.x"));
        assert!(!is_public_id("publicity"));
        for bad in [
            "",
            ".a",
            "a.",
            "a..b",
            "../up",
            "a/b",
            "a b",
            &"x".repeat(65),
        ] {
            assert!(public_id(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_backslash_is_escaped_because_browsers_read_it_as_a_slash() {
        assert_eq!(path_segment(r"a\b"), "a%5Cb");
    }
}
