//! How times, lengths and values read on the page.

use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::BorrowedFormatItem;
use time::macros::format_description;

const SECOND: i64 = 1_000_000_000;
const TIME: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
const MINUTE: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day] [hour]:[minute]");
const DATE: &[BorrowedFormatItem<'_>] = format_description!("[year]-[month]-[day]");
const CLOCK: &[BorrowedFormatItem<'_>] = format_description!("[hour]:[minute]:[second]");

/// A length in its two largest units: "41 s", "1 min 03 s", "2 h 05 min", "3 d 4 h", "20 min".
pub fn duration_text(nanos: i64) -> String {
    if nanos <= 0 {
        return "0 s".to_owned();
    }
    if nanos < SECOND {
        return format!("{} ms", nanos / 1_000_000);
    }
    let seconds = nanos / SECOND;
    match seconds {
        0..60 => format!("{seconds} s"),
        60..3_600 => two_units((seconds / 60, "min"), (seconds % 60, "s")),
        3_600..86_400 => two_units((seconds / 3_600, "h"), (seconds % 3_600 / 60, "min")),
        _ => two_units((seconds / 86_400, "d"), (seconds % 86_400 / 3_600, "h")),
    }
}

/// "2 h 05 min", or "2 h" when the smaller unit is zero; days take hours unpadded.
fn two_units((major, big): (i64, &str), (minor, small): (i64, &str)) -> String {
    match (minor, big) {
        (0, _) => format!("{major} {big}"),
        (_, "d") => format!("{major} {big} {minor} {small}"),
        _ => format!("{major} {big} {minor:02} {small}"),
    }
}

/// A person as people read them: an email's name before the `@`.
pub fn person_text(person: &str) -> &str {
    person.split_once('@').map_or(person, |(name, _)| name)
}

/// UTC, to the second.
pub fn time_text(nanos: i64) -> String {
    formatted(nanos, TIME)
}

pub fn minute_text(nanos: i64) -> String {
    formatted(nanos, MINUTE)
}

pub fn date_text(nanos: i64) -> String {
    formatted(nanos, DATE)
}

pub fn clock_text(nanos: i64) -> String {
    formatted(nanos, CLOCK)
}

/// A JSON value as a person reads it: text without its quotes.
pub fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Every time a room holds formats; the raw number is the fallback for one that does not.
/// Integers may arrive as floats from the browser side; accept the integer-valued ones
/// that a float holds exactly.
pub fn whole_number(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        let f = value.as_f64()?;
        (f.fract() == 0.0 && f.abs() < 9.0e15).then_some(f as i64)
    })
}

fn formatted(nanos: i64, format: &[BorrowedFormatItem<'_>]) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(nanos))
        .ok()
        .and_then(|time| time.format(format).ok())
        .unwrap_or_else(|| nanos.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::fixture::{DAY, DAY0, SECOND};

    #[test]
    fn durations_read_in_their_two_largest_units() {
        assert_eq!(duration_text(41 * SECOND), "41 s");
        assert_eq!(duration_text(63 * SECOND), "1 min 03 s");
        assert_eq!(duration_text((2 * 3600 + 5 * 60) * SECOND), "2 h 05 min");
        assert_eq!(duration_text(3 * DAY + 4 * 3600 * SECOND), "3 d 4 h");
    }

    #[test]
    fn a_round_duration_drops_its_zero_unit() {
        assert_eq!(duration_text(20 * 60 * SECOND), "20 min");
        assert_eq!(duration_text(2 * 3600 * SECOND), "2 h");
        assert_eq!(duration_text(3 * DAY), "3 d");
    }

    #[test]
    fn a_person_reads_as_the_name_before_the_at() {
        assert_eq!(person_text("bob.durand@example.org"), "bob.durand");
        assert_eq!(person_text("AMDA"), "AMDA");
    }

    #[test]
    fn short_durations_read_in_milliseconds() {
        assert_eq!(duration_text(350_000_000), "350 ms");
        assert_eq!(duration_text(0), "0 s");
    }

    #[test]
    fn times_read_to_the_second() {
        assert_eq!(
            time_text(DAY0 + 14 * 3600 * SECOND + 1),
            "2019-11-03 14:00:00"
        );
        assert_eq!(date_text(DAY0 + 14 * 3600 * SECOND), "2019-11-03");
    }

    #[test]
    fn values_read_as_plain_text() {
        assert_eq!(value_text(&json!("MMS1")), "MMS1");
        assert_eq!(value_text(&json!(2.4)), "2.4");
        assert_eq!(value_text(&json!(true)), "true");
        assert_eq!(value_text(&json!([1, 2])), "[1,2]");
    }
}
