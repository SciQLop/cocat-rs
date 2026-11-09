//! A TFCat time axis: the numbers a file writes for time, and the instants they name.
//!
//! The axis is a unit counted from an origin in a time scale (spec §4.1.2, appendix B).
//! On the UTC-like scales a day is 86 400 seconds, as Julian dates and Unix time count it;
//! the other scales go through hifitime, which knows the leap seconds.

use hifitime::{Duration, Epoch, TimeScale};
use serde::Deserialize;
use serde_json::Value;
use time::PrimitiveDateTime;
use time::format_description::BorrowedFormatItem;
use time::macros::format_description;

use super::TfcatError;
use crate::time::Timestamp;

#[derive(Deserialize)]
struct Crs {
    #[serde(rename = "type")]
    kind: String,
    properties: Local,
}

#[derive(Deserialize)]
struct Local {
    time_coords_id: Option<Predefined>,
    time_coords: Option<TimeCoords>,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Predefined {
    Unix,
    Jd,
    Mjd,
    MjdNasa,
    MjdCnes,
    #[serde(rename = "cdf_tt2000")]
    CdfTt2000,
}

#[derive(Deserialize)]
struct TimeCoords {
    unit: String,
    time_origin: String,
    time_scale: String,
}

impl Predefined {
    /// Unit, origin and scale, from the spec's appendix B.
    fn coords(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Predefined::Unix => ("s", "1970-01-01T00:00:00", "UTC"),
            // The spec's -4712-01-01T12:00 is in the Julian calendar; this is the same day in
            // the proleptic Gregorian calendar the parser reads.
            Predefined::Jd => ("d", "-4713-11-24T12:00:00", "UTC"),
            Predefined::Mjd => ("d", "1858-11-17T00:00:00", "UTC"),
            Predefined::MjdNasa => ("d", "1968-05-24T00:00:00", "UTC"),
            Predefined::MjdCnes => ("d", "1950-01-01T00:00:00", "UTC"),
            Predefined::CdfTt2000 => ("ns", "2000-01-01T00:00:00", "TT"),
        }
    }
}

const UNITS: [(&str, i128); 7] = [
    ("ns", 1),
    ("us", 1_000),
    ("ms", 1_000_000),
    ("s", 1_000_000_000),
    ("min", 60_000_000_000),
    ("h", 3_600_000_000_000),
    ("d", 86_400_000_000_000),
];

const ORIGIN: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second][optional [.[subsecond]]]");

enum Origin {
    /// Nanoseconds since 1970 on a day of 86 400 seconds.
    Posix(i128),
    Scale(Epoch),
}

pub(super) struct TimeAxis {
    origin: Origin,
    /// Nanoseconds in one unit.
    unit: i128,
}

impl TimeAxis {
    pub(super) fn of(crs: &Value) -> Result<Self, TfcatError> {
        let crs: Crs = serde_json::from_value(crs.clone())
            .map_err(|error| TfcatError::Crs(error.to_string()))?;
        if crs.kind != "local" {
            return Err(TfcatError::Crs(
                "only a \"local\" coordinate system says how to read times".into(),
            ));
        }
        match (crs.properties.time_coords_id, crs.properties.time_coords) {
            (Some(id), _) => {
                let (unit, origin, scale) = id.coords();
                Self::new(unit, origin, scale)
            }
            (None, Some(coords)) => {
                Self::new(&coords.unit, &coords.time_origin, &coords.time_scale)
            }
            (None, None) => Err(TfcatError::Crs(
                "it has neither time_coords_id nor time_coords".into(),
            )),
        }
    }

    fn new(unit: &str, origin: &str, scale: &str) -> Result<Self, TfcatError> {
        let unit = UNITS
            .iter()
            .find(|(name, _)| *name == unit)
            .map(|(_, nanos)| *nanos)
            .ok_or_else(|| TfcatError::Crs(format!("time unit {unit:?}")))?;
        let date = origin_date(origin)
            .ok_or_else(|| TfcatError::Crs(format!("time origin {origin:?}")))?;
        let origin = match time_scale(scale)? {
            None => Origin::Posix(date.assume_utc().unix_timestamp_nanos()),
            Some(scale) => Origin::Scale(epoch(date, scale)?),
        };
        Ok(Self { origin, unit })
    }

    pub(super) fn instant(&self, value: f64) -> Result<Timestamp, TfcatError> {
        let unix = nanos(value, self.unit).and_then(|offset| match self.origin {
            Origin::Posix(origin) => origin.checked_add(offset),
            Origin::Scale(origin) => Some(
                (origin + Duration::from_total_nanoseconds(offset))
                    .to_unix_duration()
                    .total_nanoseconds(),
            ),
        });
        unix.and_then(|unix| i64::try_from(unix).ok())
            .and_then(Timestamp::from_unix_nanos)
            .ok_or(TfcatError::OutOfRange(value))
    }

    pub(super) fn value(&self, time: Timestamp) -> Result<f64, TfcatError> {
        let unix = i128::from(time.unix_nanos().ok_or(TfcatError::Time(time))?);
        let offset = match self.origin {
            Origin::Posix(origin) => unix - origin,
            Origin::Scale(origin) => {
                (Epoch::from_unix_duration(Duration::from_total_nanoseconds(unix)) - origin)
                    .total_nanoseconds()
            }
        };
        Ok(units(offset, self.unit))
    }
}

/// `None` for the scales read as UTC: a spacecraft event time is UTC on board, and an
/// unknown scale is taken as the usual one.
fn time_scale(name: &str) -> Result<Option<TimeScale>, TfcatError> {
    Ok(Some(match name {
        "UTC" | "UT" | "UNKNOWN" | "SCET" => return Ok(None),
        "TAI" => TimeScale::TAI,
        "TT" => TimeScale::TT,
        "TDB" => TimeScale::TDB,
        "TCB" => TimeScale::TCB,
        "TCG" => TimeScale::TCG,
        "GPS" => TimeScale::GPST,
        other => return Err(TfcatError::Crs(format!("time scale {other:?}"))),
    }))
}

/// `[-]YYYY-MM-DD['T'hh:mm:ss[.SSS]]` (spec §4.1.2), also with the space and the `Z` the
/// reference Python package writes.
fn origin_date(text: &str) -> Option<PrimitiveDateTime> {
    let text = text.trim().trim_end_matches('Z').replacen(' ', "T", 1);
    let text = match text.contains('T') {
        true => text,
        false => format!("{text}T00:00:00"),
    };
    PrimitiveDateTime::parse(&text, ORIGIN).ok()
}

fn epoch(date: PrimitiveDateTime, scale: TimeScale) -> Result<Epoch, TfcatError> {
    Epoch::maybe_from_gregorian(
        date.year(),
        date.month() as u8,
        date.day(),
        date.hour(),
        date.minute(),
        date.second(),
        date.nanosecond(),
        scale,
    )
    .map_err(|error| TfcatError::Crs(error.to_string()))
}

/// The whole units and the fraction apart, so a large count keeps its nanoseconds.
fn nanos(value: f64, unit: i128) -> Option<i128> {
    if !value.is_finite() || value.abs() > 1e30 {
        return None;
    }
    let whole = (value.trunc() as i128).checked_mul(unit)?;
    whole.checked_add((value.fract() * unit as f64).round() as i128)
}

fn units(offset: i128, unit: i128) -> f64 {
    offset.div_euclid(unit) as f64 + offset.rem_euclid(unit) as f64 / unit as f64
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::TimeAxis;

    fn axis(properties: serde_json::Value) -> TimeAxis {
        TimeAxis::of(&json!({"type": "local", "properties": properties})).unwrap()
    }

    fn by_id(id: &str) -> TimeAxis {
        axis(
            json!({"time_coords_id": id, "spectral_coords": {"type": "frequency", "unit": "Hz"},
                    "ref_position_id": "GEOCENTER"}),
        )
    }

    fn custom(unit: &str, origin: &str, scale: &str) -> TimeAxis {
        axis(
            json!({"time_coords": {"unit": unit, "time_origin": origin, "time_scale": scale},
                    "spectral_coords": {"type": "frequency", "unit": "Hz"},
                    "ref_position_id": "GEOCENTER"}),
        )
    }

    #[test]
    fn each_time_coordinate_system_names_its_instant_and_back() {
        let cases = [
            (by_id("unix"), 1158051858.75, "2006-09-12T09:04:18.750000Z"),
            (by_id("jd"), 2451545.0, "2000-01-01T12:00:00.000000Z"),
            (by_id("mjd"), 51544.5, "2000-01-01T12:00:00.000000Z"),
            (by_id("mjd_nasa"), 11544.5, "2000-01-01T12:00:00.000000Z"),
            (by_id("mjd_cnes"), 18262.5, "2000-01-01T12:00:00.000000Z"),
            // TT runs 64.184 s ahead of UTC in 2000.
            (by_id("cdf_tt2000"), 0.0, "1999-12-31T23:58:55.816000Z"),
            (
                custom("h", "2000-01-01", "UTC"),
                12.0,
                "2000-01-01T12:00:00.000000Z",
            ),
            (
                custom("s", "1970-01-01T00:00:00.000Z", "UTC"),
                1.5,
                "1970-01-01T00:00:01.500000Z",
            ),
            // TAI runs 37 s ahead of UTC since 2017.
            (
                custom("s", "2020-01-01T00:00:00", "TAI"),
                37.0,
                "2020-01-01T00:00:00.000000Z",
            ),
            (
                custom("ms", "1980-01-06 00:00:00", "GPS"),
                0.0,
                "1980-01-06T00:00:00.000000Z",
            ),
        ];
        for (axis, value, expected) in cases {
            let instant = axis.instant(value).unwrap();
            assert_eq!(instant.to_string(), expected, "value {value}");
            let back = axis.value(instant).unwrap();
            assert!(
                (back - value).abs() < 1e-9 * value.abs().max(1.0),
                "{back} != {value}"
            );
        }
    }

    #[test]
    fn a_value_read_and_written_again_is_the_same_number() {
        let axis = by_id("unix");
        for value in [1158051858.75, 1340700754.66, 1483343656.509, 0.1] {
            assert_eq!(axis.value(axis.instant(value).unwrap()).unwrap(), value);
        }
        let jd = by_id("jd");
        assert_eq!(
            jd.value(jd.instant(2460000.123).unwrap()).unwrap(),
            2460000.123
        );
    }

    #[test]
    fn an_axis_cocat_cannot_read_is_refused_in_words() {
        let refused = [
            json!({"type": "name", "properties": {"name": "x"}}),
            json!({"type": "local", "properties": {"spectral_coords": {}}}),
            json!({"type": "local", "properties": {"time_coords":
                {"unit": "s", "time_origin": "1970-01-01", "time_scale": "SCLK"}}}),
            json!({"type": "local", "properties": {"time_coords":
                {"unit": "fortnight", "time_origin": "1970-01-01", "time_scale": "UTC"}}}),
            json!({"type": "local", "properties": {"time_coords":
                {"unit": "s", "time_origin": "yesterday", "time_scale": "UTC"}}}),
        ];
        for crs in refused {
            assert!(TimeAxis::of(&crs).is_err(), "{crs}");
        }
    }

    #[test]
    fn a_time_outside_what_a_project_holds_is_refused() {
        assert!(by_id("unix").instant(1e15).is_err());
        assert!(by_id("unix").instant(f64::NAN).is_err());
    }
}
