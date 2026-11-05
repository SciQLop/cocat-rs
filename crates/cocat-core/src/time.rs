//! Timestamps as cocat stores them.
//!
//! Python writes `str(datetime)`: a space between the date and the time, an
//! optional fraction, and an offset only when the value is timezone-aware. A
//! value without one is UTC, which is what cocat's own clients assume.
//!
//! Reading normalises that text to RFC 3339 and prints back one shape only
//! (`2025-01-31T00:00:00.000000Z`), so the same instant is the same text on
//! every server.

use std::fmt;

use serde::{Serialize, Serializer};
use time::OffsetDateTime;
use time::UtcOffset;
use time::format_description::BorrowedFormatItem;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;

const OUTPUT: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]Z");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(OffsetDateTime);

impl Timestamp {
    pub fn parse(text: &str) -> Option<Self> {
        let text = normalise(text)?;
        let parsed = OffsetDateTime::parse(&text, &Rfc3339).ok()?;
        Some(Self(parsed.to_offset(UtcOffset::UTC)))
    }

    pub fn unix_micros(self) -> i128 {
        self.0.unix_timestamp_nanos() / 1_000
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.format(OUTPUT) {
            Ok(text) => f.write_str(&text),
            Err(_) => Err(fmt::Error),
        }
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

fn normalise(text: &str) -> Option<String> {
    let text = text.trim();
    let (date, rest) = text.split_once([' ', 'T'])?;

    let mut out = String::with_capacity(text.len() + 1);
    out.push_str(date);
    out.push('T');
    out.push_str(rest);

    if has_offset(rest) {
        if out.ends_with('z') {
            out.pop();
            out.push('Z');
        }
    } else {
        out.push('Z');
    }
    Some(out)
}

fn has_offset(rest: &str) -> bool {
    rest.contains(['+', '-']) || rest.ends_with(['Z', 'z'])
}

#[cfg(test)]
mod tests {
    use super::Timestamp;

    #[test]
    fn a_missing_offset_is_utc() {
        let parsed = Timestamp::parse("2025-01-31 00:00:00").unwrap();
        assert_eq!(parsed.to_string(), "2025-01-31T00:00:00.000000Z");
    }

    #[test]
    fn an_offset_is_applied() {
        let parsed = Timestamp::parse("2025-01-31 02:00:00+02:00").unwrap();
        assert_eq!(parsed.to_string(), "2025-01-31T00:00:00.000000Z");
    }

    #[test]
    fn a_fraction_keeps_its_microseconds() {
        let parsed = Timestamp::parse("2025-01-31T01:02:03.0045Z").unwrap();
        assert_eq!(parsed.to_string(), "2025-01-31T01:02:03.004500Z");
    }
}
