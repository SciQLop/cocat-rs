//! A time relative to when a rule is evaluated: `{"now": "-7d"}`. The offset is a sign, a
//! whole number and a unit (s, m, h, d, M, y); the empty offset is now itself.

use serde_json::Value;

use crate::time::Timestamp;

/// The units of an offset: seconds for s, m, h, d; calendar months for M and y.
const UNITS: [(char, Step); 6] = [
    ('s', Step::Seconds(1)),
    ('m', Step::Seconds(60)),
    ('h', Step::Seconds(3_600)),
    ('d', Step::Seconds(86_400)),
    ('M', Step::Months(1)),
    ('y', Step::Months(12)),
];

#[derive(Clone, Copy)]
enum Step {
    Seconds(i64),
    Months(i64),
}

/// The offset text of a `{"now": offset}` value.
pub fn offset_of(value: &Value) -> Option<&str> {
    let object = value.as_object()?;
    match (object.len(), object.get("now")) {
        (1, Some(Value::String(offset))) => Some(offset),
        _ => None,
    }
}

/// Whether `value` is meant as a relative time, well-formed or not.
pub fn is_relative(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.contains_key("now"))
}

pub fn is_offset(offset: &str) -> bool {
    offset.is_empty() || step(offset).is_some()
}

/// The instant `offset` from `now`, in nanoseconds, or `None` when the offset is malformed
/// or lands outside 1677–2262.
pub fn resolve(offset: &str, now: Timestamp) -> Option<i64> {
    if offset.is_empty() {
        return now.unix_nanos();
    }
    match step(offset)? {
        Step::Seconds(seconds) => now
            .unix_nanos()?
            .checked_add(seconds.checked_mul(1_000_000_000)?),
        Step::Months(months) => now.add_months(months)?.unix_nanos(),
    }
}

/// The signed offset, in its unit's steps.
fn step(offset: &str) -> Option<Step> {
    let sign = match offset.chars().next()? {
        '-' => -1,
        '+' => 1,
        _ => return None,
    };
    let unit = offset.chars().last()?;
    let digits = &offset[1..offset.len() - unit.len_utf8()];
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let amount = sign * digits.parse::<i64>().ok()?;
    let (_, step) = UNITS.iter().find(|(name, _)| *name == unit)?;
    Some(match step {
        Step::Seconds(size) => Step::Seconds(amount.checked_mul(*size)?),
        Step::Months(size) => Step::Months(amount.checked_mul(*size)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_take_a_sign_a_number_and_a_unit() {
        for good in ["", "-7d", "+1h", "-30s", "-2m", "-1M", "+3y"] {
            assert!(is_offset(good), "{good}");
        }
        for bad in ["7d", "-d", "-7", "-7w", "-1.5d", "now", "- 7d", "-7dd"] {
            assert!(!is_offset(bad), "{bad}");
        }
    }

    #[test]
    fn an_offset_moves_now() {
        let now = Timestamp::parse("2025-03-31 12:00:00").unwrap();
        let moved = |offset| Timestamp::from_unix_nanos(resolve(offset, now).unwrap()).unwrap();
        assert_eq!(moved(""), now);
        assert_eq!(moved("-1d").to_string(), "2025-03-30T12:00:00.000000Z");
        assert_eq!(moved("+90m").to_string(), "2025-03-31T13:30:00.000000Z");
        assert_eq!(moved("-1M").to_string(), "2025-02-28T12:00:00.000000Z");
        assert_eq!(moved("-1y").to_string(), "2024-03-31T12:00:00.000000Z");
        assert_eq!(resolve("-999999999999y", now), None);
    }
}
