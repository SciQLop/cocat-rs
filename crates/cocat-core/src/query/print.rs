//! A predicate as query text, with canonical spacing. A predicate the language cannot say
//! (a `!=`, a regex on tags, ...) has no text: what is printed must read back the same.

use serde_json::Value;

use super::grammar::{KEYWORDS, VALUE_STOPS, is_word_char};
use super::meaning::{Time, span};
use super::{Kind, canonical, named};
use crate::rules::{Op, Predicate, relative};
use crate::time::Timestamp;

/// The units a duration prints in, largest first.
const DURATION_UNITS: [(&str, f64); 3] = [("d", 86_400.0), ("h", 3_600.0), ("min", 60.0)];

/// The query text of `predicate`, or `None` when it has none.
pub fn print(predicate: &Predicate, catalogues: &[(String, String)]) -> Option<String> {
    let predicate = canonical(predicate.clone());
    let text = Printer { catalogues }.or(&predicate)?;
    let reads_back = super::parse(&text, catalogues).is_ok_and(|read| read == predicate);
    reads_back.then_some(text)
}

struct Printer<'c> {
    catalogues: &'c [(String, String)],
}

impl Printer<'_> {
    fn or(&self, predicate: &Predicate) -> Option<String> {
        match predicate {
            Predicate::Or(clauses) => joined(clauses.iter().map(|clause| self.and(clause)), " OR "),
            _ => self.and(predicate),
        }
    }

    fn and(&self, predicate: &Predicate) -> Option<String> {
        let Predicate::And(clauses) = predicate else {
            return self.unary(predicate);
        };
        let mut parts = Vec::new();
        let mut index = 0;
        while index < clauses.len() {
            let pair = clauses
                .get(index + 1)
                .and_then(|next| self.span(&clauses[index], next));
            if let Some(span) = pair {
                parts.push(span);
                index += 2;
            } else {
                parts.push(self.unary(&clauses[index])?);
                index += 1;
            }
        }
        Some(parts.join(" "))
    }

    fn unary(&self, predicate: &Predicate) -> Option<String> {
        match predicate {
            Predicate::Not(inner) if **inner == Predicate::InCatalogue(None) => {
                Some("_exists_:catalogue".to_owned())
            }
            Predicate::Not(inner) => Some(format!("NOT {}", self.unary(inner)?)),
            Predicate::And(_) | Predicate::Or(_) => Some(format!("({})", self.or(predicate)?)),
            _ => self.atom(predicate),
        }
    }

    fn atom(&self, predicate: &Predicate) -> Option<String> {
        match predicate {
            Predicate::Text(word) => Some(word_text(word)),
            Predicate::Has(field) => Some(format!("_exists_:{}", named(field)?.0)),
            Predicate::InCatalogue(None) => Some("NOT _exists_:catalogue".to_owned()),
            Predicate::InCatalogue(Some(uuid)) => {
                Some(format!("catalogue:{}", self.catalogue(uuid)))
            }
            Predicate::Contains(field, Value::String(word)) => {
                let (name, kind) = named(field)?;
                (kind == Kind::Tags).then(|| format!("{name}:{}", value_text(word)))
            }
            Predicate::Match(field, pattern) => {
                Some(format!("{}:{}", named(field)?.0, pattern_text(pattern)))
            }
            Predicate::Cmp(field, op, value) => comparison(field, *op, value),
            _ => None,
        }
    }

    fn catalogue(&self, uuid: &str) -> String {
        let name = self.catalogues.iter().find(|(known, _)| known == uuid);
        value_text(name.map_or(uuid, |(_, name)| name))
    }

    /// `start:2015` for the bounds a year, a month or a day reads into.
    fn span(&self, first: &Predicate, second: &Predicate) -> Option<String> {
        let (Predicate::Cmp(field, Op::Ge, low), Predicate::Cmp(other, Op::Lt, high)) =
            (first, second)
        else {
            return None;
        };
        let (name, kind) = named(field)?;
        if field != other || kind != Kind::Time {
            return None;
        }
        let (low, high) = (low.as_i64()?, high.as_i64()?);
        let date = Timestamp::from_unix_nanos(low)?.rfc3339().ok()?;
        let text = [4, 7, 10]
            .into_iter()
            .filter_map(|length| date.get(..length))
            .find(|text| span(text) == Some(Time::Span(low, high)))?;
        Some(format!("{name}:{text}"))
    }
}

fn joined(parts: impl Iterator<Item = Option<String>>, joint: &str) -> Option<String> {
    Some(parts.collect::<Option<Vec<_>>>()?.join(joint))
}

fn comparison(field: &str, op: Op, value: &Value) -> Option<String> {
    let (name, kind) = named(field)?;
    let text = match kind {
        Kind::Time => time_text(op, value)?,
        Kind::Duration => duration_text(value.as_f64()?),
        _ => match value {
            Value::String(text) => value_text(text),
            Value::Number(number) => number.to_string(),
            _ => return None,
        },
    };
    let exact = matches!(kind, Kind::Text | Kind::Attribute) && value.is_string();
    let symbol = match op {
        Op::Eq if exact => "=",
        Op::Eq => "",
        Op::Gt => ">",
        Op::Ge => ">=",
        Op::Lt => "<",
        Op::Le => "<=",
        Op::Ne => return None,
    };
    Some(format!("{name}:{symbol}{text}"))
}

/// A date alone where the operator takes a span's start, else the full instant.
fn time_text(op: Op, value: &Value) -> Option<String> {
    if let Some(offset) = relative::offset_of(value) {
        return Some(format!("now{offset}"));
    }
    let instant = Timestamp::from_unix_nanos(value.as_i64()?)?
        .rfc3339()
        .ok()?;
    match instant.strip_suffix("T00:00:00Z") {
        Some(date) if matches!(op, Op::Ge | Op::Lt) => Some(date.to_owned()),
        _ => Some(instant),
    }
}

fn duration_text(seconds: f64) -> String {
    let unit = DURATION_UNITS
        .iter()
        .find(|(_, size)| seconds != 0.0 && (seconds / size).fract() == 0.0);
    match unit {
        Some((unit, size)) => format!("{}{unit}", seconds / size),
        None => format!("{seconds}"),
    }
}

/// The contains form, `field:text`, for the pattern a query writes for it; else `/regex/`.
fn pattern_text(pattern: &str) -> String {
    let contained = pattern
        .strip_prefix("(?i)")
        .map(unescaped)
        .filter(|text| pattern == format!("(?i){}", regex::escape(text)));
    match contained {
        Some(text) => value_text(&text),
        None => format!("/{}/", pattern.replace('/', "\\/")),
    }
}

/// `regex::escape` undone: a backslash keeps the character after it.
fn unescaped(escaped: &str) -> String {
    let mut text = String::new();
    let mut characters = escaped.chars();
    while let Some(c) = characters.next() {
        text.extend(if c == '\\' {
            characters.next()
        } else {
            Some(c)
        });
    }
    text
}

fn word_text(word: &str) -> String {
    let bare = !word.is_empty()
        && word.chars().all(|c| is_word_char(c) && c != ':')
        && !KEYWORDS.contains(&word);
    if bare { word.to_owned() } else { quoted(word) }
}

/// Quoted unless it reads alone as the same value: not a number, an operator, `*` or a
/// keyword.
fn value_text(value: &str) -> String {
    let bare = !value.is_empty()
        && value
            .chars()
            .all(|c| !c.is_whitespace() && !VALUE_STOPS.contains(&c))
        && !value.starts_with(['>', '<', '=', '/'])
        && value != "*"
        && value != "TO"
        && !KEYWORDS.contains(&value)
        && value.parse::<f64>().is_err();
    if bare {
        value.to_owned()
    } else {
        quoted(value)
    }
}

fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}
