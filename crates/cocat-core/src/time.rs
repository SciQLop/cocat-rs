//! Timestamps as cocat stores them.
//!
//! Python writes `str(datetime)`: a space between the date and the time, an
//! optional fraction, and an offset only when the value is timezone-aware.
//! Reading accepts both forms, with or without an offset, and a value without
//! one is UTC. Writing always uses the aware UTC form (`+00:00`): a naive
//! datetime next to an aware one makes Python raise when the two are compared.
//!
//! Reading normalises that text to RFC 3339 and prints back one shape only
//! (`2025-01-31T00:00:00.000000Z`), so the same instant is the same text on
//! every server.

use std::fmt;

use serde::{Serialize, Serializer};
use time::UtcOffset;
use time::error::Format;
use time::format_description::BorrowedFormatItem;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;
use time::{Date, Month, OffsetDateTime};

const OUTPUT: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]Z");
const SECONDS: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
const PYTHON: &[BorrowedFormatItem<'_>] = format_description!(
    "[year]-[month]-[day] [hour]:[minute]:[second][offset_hour sign:mandatory]:[offset_minute]"
);
const PYTHON_FRACTION: &[BorrowedFormatItem<'_>] = format_description!(
    "[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:6][offset_hour sign:mandatory]:[offset_minute]"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(OffsetDateTime);

/// The calendar fields of an instant, as a `datetime` constructor takes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parts {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub microsecond: u32,
}

impl Timestamp {
    pub fn parse(text: &str) -> Option<Self> {
        let text = normalise(text)?;
        let parsed = OffsetDateTime::parse(&text, &Rfc3339).ok()?;
        Some(Self(parsed.to_offset(UtcOffset::UTC)))
    }

    pub fn now() -> Self {
        Self(OffsetDateTime::now_utc())
    }

    /// The same day and time `months` calendar months later (earlier when negative); a day
    /// the target month lacks becomes its last day, so a month after 31 January is 28 or
    /// 29 February.
    pub fn add_months(self, months: i64) -> Option<Self> {
        let index = i64::from(self.0.year()) * 12 + i64::from(self.0.month() as u8) - 1;
        let index = index.checked_add(months)?;
        let year = i32::try_from(index.div_euclid(12)).ok()?;
        let month = Month::try_from(u8::try_from(index.rem_euclid(12) + 1).ok()?).ok()?;
        let day = self.0.day().min(time::util::days_in_month(month, year));
        let date = Date::from_calendar_date(year, month, day).ok()?;
        Some(Self(self.0.replace_date(date)))
    }

    /// UTC to the second, as the web app's event table shows a time.
    pub fn seconds_text(self) -> Result<String, Format> {
        self.0.format(SECONDS)
    }

    pub fn unix_micros(self) -> i128 {
        self.0.unix_timestamp_nanos() / 1_000
    }

    /// The instant as microseconds since the Unix epoch, or `None` when it does not fit the
    /// range a datetime can hold.
    pub fn from_unix_micros(micros: i128) -> Option<Self> {
        let nanos = micros.checked_mul(1_000)?;
        OffsetDateTime::from_unix_timestamp_nanos(nanos)
            .ok()
            .map(|instant| Self(instant.to_offset(UtcOffset::UTC)))
    }

    /// Nanoseconds since the Unix epoch, the form a room stores, or `None` outside the
    /// 1677–2262 range an `i64` holds.
    pub fn unix_nanos(self) -> Option<i64> {
        i64::try_from(self.0.unix_timestamp_nanos()).ok()
    }

    pub fn from_unix_nanos(nanos: i64) -> Option<Self> {
        OffsetDateTime::from_unix_timestamp_nanos(i128::from(nanos))
            .ok()
            .map(|instant| Self(instant.to_offset(UtcOffset::UTC)))
    }

    /// The calendar fields Python's `datetime` takes, so a binding can build one.
    pub fn parts(self) -> Parts {
        Parts {
            year: self.0.year(),
            month: self.0.month() as u8,
            day: self.0.day(),
            hour: self.0.hour(),
            minute: self.0.minute(),
            second: self.0.second(),
            microsecond: self.0.microsecond(),
        }
    }

    /// The text Python's `str(datetime)` writes for the same instant: a space between the
    /// date and the time, microseconds only when there are any, and the `+00:00` offset of
    /// the aware UTC datetime cocat writes. The offset is always written because Python
    /// raises when a naive datetime is compared with an aware one, so a document mixing
    /// the two breaks older clients; every instant here is already normalised to UTC.
    pub fn as_text(self) -> Result<String, Format> {
        let format = match self.0.microsecond() {
            0 => PYTHON,
            _ => PYTHON_FRACTION,
        };
        self.0.format(format)
    }

    /// RFC 3339 in UTC, with as many fraction digits as the instant needs, down to the
    /// nanosecond a room stores.
    pub fn rfc3339(self) -> Result<String, Format> {
        self.0.format(&Rfc3339)
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
    // A bare date is midnight, as `datetime.fromisoformat` takes it.
    let (date, rest) = match text.split_once([' ', 'T']) {
        Some(parts) => parts,
        None => (text, "00:00:00"),
    };

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
    fn a_bare_date_is_midnight_utc() {
        let parsed = Timestamp::parse("2025-01-30").unwrap();
        assert_eq!(parsed.to_string(), "2025-01-30T00:00:00.000000Z");
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

    #[test]
    fn python_text_is_an_aware_utc_datetime() {
        let utc = Timestamp::parse("2025-01-31 11:00:00").unwrap();
        assert_eq!(utc.as_text().unwrap(), "2025-01-31 11:00:00+00:00");

        let precise = Timestamp::parse("2025-01-31T11:00:00.123456Z").unwrap();
        assert_eq!(
            precise.as_text().unwrap(),
            "2025-01-31 11:00:00.123456+00:00"
        );

        let offset = Timestamp::parse("2025-01-31 13:00:00+02:00").unwrap();
        assert_eq!(offset.as_text().unwrap(), "2025-01-31 11:00:00+00:00");
    }

    #[test]
    fn nanoseconds_round_trip() {
        let precise = Timestamp::parse("2025-01-31T11:00:00.123456Z").unwrap();
        let nanos = precise.unix_nanos().unwrap();
        assert_eq!(nanos, 1_738_321_200_123_456_000);
        assert_eq!(Timestamp::from_unix_nanos(nanos), Some(precise));
    }

    #[test]
    fn a_month_later_keeps_the_day_or_takes_the_last_one() {
        let end = Timestamp::parse("2024-01-31 12:00:00").unwrap();
        let month = |months| end.add_months(months).unwrap().to_string();
        assert_eq!(month(1), "2024-02-29T12:00:00.000000Z");
        assert_eq!(month(-2), "2023-11-30T12:00:00.000000Z");
        assert_eq!(month(-12), "2023-01-31T12:00:00.000000Z");
        assert_eq!(end.add_months(i64::MAX), None);
    }

    #[test]
    fn an_instant_past_2262_has_no_nanoseconds() {
        let far = Timestamp::parse("2300-01-01 00:00:00").unwrap();
        assert_eq!(far.unix_nanos(), None);
    }

    #[test]
    fn both_python_forms_read_back_to_the_same_instant() {
        let naive = Timestamp::parse("2025-01-31 11:00:00").unwrap();
        let aware = Timestamp::parse("2025-01-31 11:00:00+00:00").unwrap();
        assert_eq!(naive, aware);
        assert_eq!(naive.unix_micros(), aware.unix_micros());
    }
}
