//! Data and rate units, spelled as VOUnits does.

use winnow::combinator::{alt, opt, preceded};
use winnow::prelude::*;

// simplify: a VOUnits subset (data and time only); a full VOUnits/QUDT parser when budgets need other quantities.

/// A data unit, and the time it is divided by when it is a rate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Unit {
    /// Bits in one unit.
    pub bits: f64,
    /// Seconds in the time divisor, when the unit is a rate.
    pub per_seconds: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not a data unit or rate (like byte, kbit, Mibyte/min)")]
pub struct UnitError(pub String);

pub fn parse_unit(notation: &str) -> Result<Unit, UnitError> {
    unit.parse(notation)
        .map_err(|_| UnitError(notation.to_owned()))
}

fn unit(input: &mut &str) -> winnow::Result<Unit> {
    let (prefix, bits, per_seconds) =
        (opt(prefix), base, opt(preceded('/', time))).parse_next(input)?;
    Ok(Unit {
        bits: prefix.unwrap_or(1.0) * bits,
        per_seconds,
    })
}

fn prefix(input: &mut &str) -> winnow::Result<f64> {
    alt((
        alt((
            "ki".value(KIBI),
            "Ki".value(KIBI),
            "Mi".value(KIBI.powi(2)),
            "Gi".value(KIBI.powi(3)),
            "Ti".value(KIBI.powi(4)),
            "Pi".value(KIBI.powi(5)),
            "Ei".value(KIBI.powi(6)),
        )),
        alt((
            "k".value(1e3),
            "M".value(1e6),
            "G".value(1e9),
            "T".value(1e12),
            "P".value(1e15),
            "E".value(1e18),
        )),
    ))
    .parse_next(input)
}

const KIBI: f64 = 1024.0;

fn base(input: &mut &str) -> winnow::Result<f64> {
    alt(("bit".value(1.0), "byte".value(8.0))).parse_next(input)
}

fn time(input: &mut &str) -> winnow::Result<f64> {
    alt((
        "min".value(60.0),
        "s".value(1.0),
        "h".value(3600.0),
        "d".value(86400.0),
    ))
    .parse_next(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_units_follow_vounits() {
        assert_eq!(parse_unit("byte").unwrap().bits, 8.0);
        assert_eq!(parse_unit("kbit").unwrap().bits, 1000.0);
        assert_eq!(parse_unit("Mibyte").unwrap().bits, 8.0 * 1048576.0);
        assert!(parse_unit("MiB").is_err());
        assert!(parse_unit("Mibyte/fortnight").is_err());
    }

    #[test]
    fn a_rate_divides_by_a_time() {
        assert_eq!(parse_unit("Mibyte/min").unwrap().per_seconds, Some(60.0));
        assert_eq!(parse_unit("kbyte/s").unwrap().per_seconds, Some(1.0));
        assert_eq!(parse_unit("byte").unwrap().per_seconds, None);
    }

    #[test]
    fn hours_and_days_are_times_too() {
        assert_eq!(parse_unit("bit/h").unwrap().per_seconds, Some(3600.0));
        assert_eq!(parse_unit("bit/d").unwrap().per_seconds, Some(86400.0));
    }

    #[test]
    fn a_unit_with_trailing_text_is_refused() {
        assert!(parse_unit("byte ").is_err());
        assert!(parse_unit("").is_err());
    }
}
