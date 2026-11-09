//! What a query's tree means as a rule predicate. Each field matches by its kind: text
//! contains, tags exactly, times by the span they name.

use std::ops::Range;

use serde_json::{Value, json};

use super::grammar::{Ast, Name, Scalar, Term};
use super::{FIELDS, Kind, QueryError, resolve};
use crate::rules::{Op, Predicate, relative};
use crate::time::Timestamp;

type Result<T> = std::result::Result<T, QueryError>;

const DAY: i64 = 86_400_000_000_000;
/// The units a duration takes, in seconds.
const DURATION_UNITS: [(&str, f64); 4] =
    [("s", 1.0), ("min", 60.0), ("h", 3_600.0), ("d", 86_400.0)];

/// The predicate of a query's tree; `text` is the query, for error positions.
pub fn predicate(tree: &Ast, text: &str, catalogues: &[(String, String)]) -> Result<Predicate> {
    Meaning { text, catalogues }.of(tree)
}

struct Meaning<'q> {
    text: &'q str,
    catalogues: &'q [(String, String)],
}

impl Meaning<'_> {
    fn error(&self, at: &Range<usize>, message: String) -> QueryError {
        QueryError::at(self.text, at.start, message)
    }

    fn of(&self, tree: &Ast) -> Result<Predicate> {
        Ok(match tree {
            Ast::Or(clauses) => Predicate::Or(self.all(clauses)?),
            Ast::And(clauses) => Predicate::And(self.all(clauses)?),
            Ast::Not(inner) => Predicate::Not(Box::new(self.of(inner)?)),
            Ast::Word(word) => Predicate::Text(word.text.clone()),
            Ast::Exists(name) => self.exists(name)?,
            Ast::Field(name, term) => self.field(name, term)?,
        })
    }

    fn all(&self, clauses: &[Ast]) -> Result<Vec<Predicate>> {
        clauses.iter().map(|clause| self.of(clause)).collect()
    }

    fn resolve(&self, name: &Name) -> Result<(String, Kind)> {
        resolve(&name.text).ok_or_else(|| self.error(&name.at, unknown_field(&name.text)))
    }

    /// Present; for `catalogue`, listed by some catalogue.
    fn exists(&self, name: &Name) -> Result<Predicate> {
        Ok(match self.resolve(name)? {
            (_, Kind::Catalogue) => Predicate::Not(Box::new(Predicate::InCatalogue(None))),
            (field, _) => Predicate::Has(field),
        })
    }

    fn field(&self, name: &Name, term: &Term) -> Result<Predicate> {
        let (field, kind) = self.resolve(name)?;
        match kind {
            Kind::Text => self.text(field, term),
            Kind::Number => self.ordered(&field, term, &|value| number(value).ok_or("a number")),
            Kind::Duration => self.ordered(&field, term, &|value| duration(value).ok_or(DURATION)),
            Kind::Time => self.time(&field, term),
            Kind::Enum(values) => self.choice(&name.text, field, values, term),
            Kind::Tags => self.tag(&name.text, field, term),
            Kind::Catalogue => self.catalogue(term),
            Kind::Attribute => self.attribute(field, term),
            Kind::Presence => Err(self.error(
                &name.at,
                format!("{0} is only searched for with _exists_:{0}", name.text),
            )),
        }
    }

    /// Contains by default; `=` for the exact text.
    fn text(&self, field: String, term: &Term) -> Result<Predicate> {
        match term {
            Term::Single(None, value) => Ok(contains(field, &value.text)),
            _ => self.matched(field, term, &|value| Ok(value.into())),
        }
    }

    fn attribute(&self, field: String, term: &Term) -> Result<Predicate> {
        let value = |text: &str| Ok(number(text).unwrap_or_else(|| text.into()));
        match term {
            Term::Single(None, scalar) if scalar.quoted || number(&scalar.text).is_none() => {
                Ok(contains(field, &scalar.text))
            }
            _ => self.matched(field, term, &value),
        }
    }

    /// A regex match, or a comparison of `value`s.
    fn matched(
        &self,
        field: String,
        term: &Term,
        value: &dyn Fn(&str) -> std::result::Result<Value, &'static str>,
    ) -> Result<Predicate> {
        match term {
            Term::Regex(pattern) => self.regex(field, pattern),
            _ => self.ordered(&field, term, value),
        }
    }

    fn regex(&self, field: String, pattern: &Scalar) -> Result<Predicate> {
        regex::Regex::new(&pattern.text).map_err(|error| {
            let message = format!("/{}/ is not a regular expression: {error}", pattern.text);
            self.error(&pattern.at, message)
        })?;
        Ok(Predicate::Match(field, pattern.text.clone()))
    }

    /// Equal by default, an operator, or a range with `*` for an open end.
    fn ordered(
        &self,
        field: &str,
        term: &Term,
        value: &dyn Fn(&str) -> std::result::Result<Value, &'static str>,
    ) -> Result<Predicate> {
        let read = |scalar: &Scalar| {
            value(&scalar.text).map_err(|what| {
                self.error(
                    &scalar.at,
                    format!("expected {what}, not “{}”", scalar.text),
                )
            })
        };
        let cmp = |op, value| Predicate::Cmp(field.to_owned(), op, value);
        match term {
            Term::Single(op, scalar) => Ok(cmp(op.unwrap_or(Op::Eq), read(scalar)?)),
            Term::Range(low, high) => {
                let low = (!low.is_open()).then(|| read(low)).transpose()?;
                let high = (!high.is_open()).then(|| read(high)).transpose()?;
                let bounds = [
                    low.map(|low| cmp(Op::Ge, low)),
                    high.map(|high| cmp(Op::Le, high)),
                ];
                Ok(all_or_present(field, bounds))
            }
            Term::Regex(pattern) => Err(self.error(
                &pattern.at,
                format!("{field} is not text: a regular expression does not apply"),
            )),
        }
    }

    /// A year, month or day without an operator is the span it covers; with one, its start
    /// or its end, whichever the operator means. A bare `now-7d` means since then: nobody
    /// means the one nanosecond.
    fn time(&self, field: &str, term: &Term) -> Result<Predicate> {
        let cmp = |op, nanos: Value| Predicate::Cmp(field.to_owned(), op, nanos);
        let read = |scalar: &Scalar| {
            time(&scalar.text).ok_or_else(|| self.error(&scalar.at, bad_time(&scalar.text)))
        };
        match term {
            Term::Single(op, scalar) => Ok(match (op, read(scalar)?) {
                (None, Time::Span(low, high)) => {
                    Predicate::And(vec![cmp(Op::Ge, low.into()), cmp(Op::Lt, high.into())])
                }
                (None, Time::Instant(value)) if value.get("now").is_some() => cmp(Op::Ge, value),
                (op, time) => {
                    let (op, value) = time.bound(op.unwrap_or(Op::Eq));
                    cmp(op, value)
                }
            }),
            Term::Range(low, high) => {
                let low = (!low.is_open()).then(|| read(low)).transpose()?;
                let high = (!high.is_open()).then(|| read(high)).transpose()?;
                let bound = |op, time: Time| {
                    let (op, value) = time.bound(op);
                    cmp(op, value)
                };
                let bounds = [
                    low.map(|low| bound(Op::Ge, low)),
                    high.map(|high| bound(Op::Le, high)),
                ];
                Ok(all_or_present(field, bounds))
            }
            Term::Regex(pattern) => Err(self.error(
                &pattern.at,
                format!("{field} is a time: a regular expression does not apply"),
            )),
        }
    }

    fn choice(&self, name: &str, field: String, values: &[&str], term: &Term) -> Result<Predicate> {
        match term {
            Term::Single(None | Some(Op::Eq), scalar) => {
                let value = scalar.text.to_lowercase();
                if values.contains(&value.as_str()) {
                    Ok(Predicate::Cmp(field, Op::Eq, value.into()))
                } else {
                    let message = format!("{name} is one of {}", values.join(", "));
                    Err(self.error(&scalar.at, message))
                }
            }
            Term::Regex(pattern) => self.regex(field, pattern),
            Term::Single(_, scalar) | Term::Range(scalar, _) => Err(self.error(
                &scalar.at,
                format!("{name} is one of {}: it has no order", values.join(", ")),
            )),
        }
    }

    fn tag(&self, name: &str, field: String, term: &Term) -> Result<Predicate> {
        match term {
            Term::Single(None | Some(Op::Eq), scalar) => {
                Ok(Predicate::Contains(field, scalar.text.clone().into()))
            }
            Term::Single(_, scalar) | Term::Range(scalar, _) | Term::Regex(scalar) => Err(self
                .error(
                    &scalar.at,
                    format!("{name} takes one word, matched exactly"),
                )),
        }
    }

    /// A catalogue by name, or by uuid for one the room no longer names.
    fn catalogue(&self, term: &Term) -> Result<Predicate> {
        let scalar = match term {
            Term::Single(None | Some(Op::Eq), scalar) => scalar,
            Term::Single(_, scalar) | Term::Range(scalar, _) | Term::Regex(scalar) => {
                let message = "catalogue takes a catalogue's name".to_owned();
                return Err(self.error(&scalar.at, message));
            }
        };
        let named: Vec<&String> = self
            .catalogues
            .iter()
            .filter(|(_, name)| *name == scalar.text)
            .map(|(uuid, _)| uuid)
            .collect();
        match named.as_slice() {
            [uuid] => Ok(Predicate::InCatalogue(Some((*uuid).clone()))),
            [] if uuid::Uuid::parse_str(&scalar.text).is_ok() => {
                Ok(Predicate::InCatalogue(Some(scalar.text.clone())))
            }
            [] => Err(self.error(
                &scalar.at,
                format!("no catalogue is named “{}”", scalar.text),
            )),
            _ => Err(self.error(
                &scalar.at,
                format!(
                    "several catalogues are named “{}”: use its uuid",
                    scalar.text
                ),
            )),
        }
    }
}

/// The bounds that are set, joined; with neither, the field is present.
fn all_or_present(field: &str, bounds: [Option<Predicate>; 2]) -> Predicate {
    let mut set: Vec<Predicate> = bounds.into_iter().flatten().collect();
    match set.len() {
        0 => Predicate::Has(field.to_owned()),
        1 => set.remove(0),
        _ => Predicate::And(set),
    }
}

/// Contains `text`, ignoring case.
pub fn contains(field: String, text: &str) -> Predicate {
    Predicate::Match(field, format!("(?i){}", regex::escape(text)))
}

fn unknown_field(name: &str) -> String {
    let closest = FIELDS
        .iter()
        .map(|field| field.name)
        .min_by_key(|known| strsim::levenshtein(name, known));
    match closest {
        Some(closest) => format!("there is no field “{name}”: did you mean “{closest}”?"),
        None => format!("there is no field “{name}”"),
    }
}

fn bad_time(text: &str) -> String {
    format!(
        "“{text}” is not a time: write 2015, 2015-03, 2015-03-01, 2015-03-01T12:00:00 or now-7d"
    )
}

const DURATION: &str = "a duration such as 90, 90s, 10min, 2h or 1d";

/// A whole number as an integer, so it compares exactly.
pub fn number(text: &str) -> Option<Value> {
    let number: f64 = text.parse().ok()?;
    number.is_finite().then(|| super::whole(json!(number)))
}

/// Seconds, from a number and an optional unit.
pub fn duration(text: &str) -> Option<Value> {
    let split = text
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(text.len());
    let (amount, unit) = text.split_at(split);
    let scale = match unit {
        "" => 1.0,
        unit => DURATION_UNITS.iter().find(|(name, _)| *name == unit)?.1,
    };
    let seconds = amount.parse::<f64>().ok()? * scale;
    seconds.is_finite().then(|| super::whole(json!(seconds)))
}

/// A time a query names.
#[derive(Debug, Clone, PartialEq)]
pub enum Time {
    /// An instant: nanoseconds, or `{"now": offset}`.
    Instant(Value),
    /// `[start, stop)` in nanoseconds: a year, a month or a day.
    Span(i64, i64),
}

impl Time {
    /// The comparison `op` means against this time: `>` a span is from its end on, `<=` a
    /// span is before its end.
    fn bound(self, op: Op) -> (Op, Value) {
        match (self, op) {
            (Time::Instant(value), op) => (op, value),
            (Time::Span(_, high), Op::Gt) => (Op::Ge, high.into()),
            (Time::Span(_, high), Op::Le) => (Op::Lt, high.into()),
            (Time::Span(low, _), op) => (op, low.into()),
        }
    }
}

pub fn time(text: &str) -> Option<Time> {
    if let Some(offset) = text.strip_prefix("now") {
        return relative::is_offset(offset).then(|| Time::Instant(json!({"now": offset})));
    }
    if let Some(span) = span(text) {
        return Some(span);
    }
    let nanos = Timestamp::parse(text)?.unix_nanos()?;
    Some(Time::Instant(nanos.into()))
}

/// The span of `2015`, `2015-03` or `2015-03-01`.
pub fn span(text: &str) -> Option<Time> {
    let digits =
        |part: &str, count: usize| part.len() == count && part.chars().all(|c| c.is_ascii_digit());
    let parts: Vec<&str> = text.split('-').collect();
    let (start, stop) = match parts.as_slice() {
        [year] if digits(year, 4) => {
            let start = Timestamp::parse(&format!("{year}-01-01"))?;
            (start, start.add_months(12)?)
        }
        [year, month] if digits(year, 4) && digits(month, 2) => {
            let start = Timestamp::parse(&format!("{year}-{month}-01"))?;
            (start, start.add_months(1)?)
        }
        [year, month, day] if digits(year, 4) && digits(month, 2) && digits(day, 2) => {
            let start = Timestamp::parse(text)?;
            let stop = Timestamp::from_unix_nanos(start.unix_nanos()?.checked_add(DAY)?)?;
            (start, stop)
        }
        _ => return None,
    };
    Some(Time::Span(start.unix_nanos()?, stop.unix_nanos()?))
}
