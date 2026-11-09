//! Testing a predicate against one item. The caller reads the item's fields, so this module
//! knows nothing of the room layout.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use regex::Regex;
use serde_json::Value;

use super::relative::{offset_of, resolve};
use super::{Op, Predicate, Problem, RuleError};
use crate::time::Timestamp;

/// The name under which an item's field reader gives every text of the item as a list:
/// tags, status, author, source id, origin and attribute values.
pub const ANY_TEXT: &str = "*";

/// What a predicate needs besides the item: its regexes, compiled once, the events of each
/// catalogue it names (for `None`: the events some catalogue lists), and its times relative
/// to now, as nanoseconds.
#[derive(Debug)]
pub struct Lookups {
    now: Timestamp,
    regexes: HashMap<String, Regex>,
    catalogues: HashMap<Option<String>, HashSet<String>>,
    relative: HashMap<String, Value>,
}

impl Lookups {
    pub fn new(
        predicate: &Predicate,
        now: Timestamp,
        events_of: &mut impl FnMut(Option<&str>) -> Result<HashSet<String>, RuleError>,
    ) -> Result<Self, RuleError> {
        let mut lookups = Self {
            now,
            regexes: HashMap::new(),
            catalogues: HashMap::new(),
            relative: HashMap::new(),
        };
        lookups.add(predicate, events_of)?;
        Ok(lookups)
    }

    fn add(
        &mut self,
        predicate: &Predicate,
        events_of: &mut impl FnMut(Option<&str>) -> Result<HashSet<String>, RuleError>,
    ) -> Result<(), RuleError> {
        match predicate {
            Predicate::Match(_, pattern) if !self.regexes.contains_key(pattern) => {
                let regex = super::regex(pattern).map_err(|problem| RuleError::Invalid {
                    at: "rule".to_owned(),
                    problem,
                })?;
                self.regexes.insert(pattern.clone(), regex);
            }
            Predicate::InCatalogue(catalogue) if !self.catalogues.contains_key(catalogue) => {
                let events = events_of(catalogue.as_deref())?;
                self.catalogues.insert(catalogue.clone(), events);
            }
            Predicate::Cmp(_, _, value) => {
                if let Some(offset) = offset_of(value) {
                    let nanos = resolve(offset, self.now).ok_or_else(|| out_of_range(offset))?;
                    self.relative.insert(offset.to_owned(), nanos.into());
                }
            }
            Predicate::Not(inner) => self.add(inner, events_of)?,
            Predicate::And(all) | Predicate::Or(all) => {
                for inner in all {
                    self.add(inner, events_of)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The value a comparison compares with: a relative time as nanoseconds.
    fn operand<'v>(&'v self, value: &'v Value) -> &'v Value {
        offset_of(value)
            .and_then(|offset| self.relative.get(offset))
            .unwrap_or(value)
    }
}

fn out_of_range(offset: &str) -> RuleError {
    RuleError::Invalid {
        at: "rule".to_owned(),
        problem: Problem::Format(format!("now{offset} is not between 1677 and 2262")),
    }
}

/// Whether an item satisfies `predicate`: `event` is its uuid when it is an event, `field`
/// reads one of its fields (`None` when unset; a null value counts as unset), and
/// `field(ANY_TEXT)` its texts as a list. As in tscat, `== null` asks whether a field is
/// unset and `!= null` whether it is set; any other comparison with an unset field is false,
/// whatever the operator.
pub fn test(
    predicate: &Predicate,
    event: Option<&str>,
    field: &impl Fn(&str) -> Option<Value>,
    lookups: &Lookups,
) -> bool {
    let value = |name: &str| read(field, name);
    match predicate {
        Predicate::Cmp(name, op, Value::Null) => match op {
            Op::Eq => value(name).is_none(),
            Op::Ne => value(name).is_some(),
            _ => false,
        },
        Predicate::Cmp(name, op, right) => {
            value(name).is_some_and(|left| compare(&left, *op, lookups.operand(right)))
        }
        Predicate::Match(name, pattern) => match (value(name), lookups.regexes.get(pattern)) {
            (Some(Value::String(text)), Some(regex)) => regex.is_match(&text),
            _ => false,
        },
        Predicate::Has(name) => value(name).is_some(),
        Predicate::Contains(name, wanted) => match value(name) {
            Some(Value::Array(items)) => items.iter().any(|item| equal(item, wanted)),
            _ => false,
        },
        Predicate::Not(inner) => !test(inner, event, field, lookups),
        Predicate::And(all) => all.iter().all(|inner| test(inner, event, field, lookups)),
        Predicate::Or(any) => any.iter().any(|inner| test(inner, event, field, lookups)),
        Predicate::InCatalogue(catalogue) => in_catalogue(catalogue, event, lookups),
        Predicate::Text(word) => has_text(&word.to_lowercase(), field),
    }
}

/// A field, with `duration` computed from the range.
fn read(field: &impl Fn(&str) -> Option<Value>, name: &str) -> Option<Value> {
    let value = |name: &str| field(name).filter(|value| !value.is_null());
    match name {
        "duration" => {
            let start = value("start")?.as_i64()?;
            let stop = value("stop")?.as_i64()?;
            Some(((stop - start) as f64 / 1e9).into())
        }
        _ => value(name),
    }
}

/// `word` is lower case.
fn has_text(word: &str, field: &impl Fn(&str) -> Option<Value>) -> bool {
    let begins_start = field("start")
        .and_then(|start| Timestamp::from_unix_nanos(start.as_i64()?))
        .and_then(|start| start.seconds_text().ok())
        .is_some_and(|start| start.starts_with(word));
    let contains = |text: &str| text.to_lowercase().contains(word);
    begins_start
        || matches!(field(ANY_TEXT), Some(Value::Array(texts))
            if texts.iter().filter_map(Value::as_str).any(contains))
}

/// A derived interval is in no catalogue.
fn in_catalogue(catalogue: &Option<String>, event: Option<&str>, lookups: &Lookups) -> bool {
    let Some(uuid) = event else {
        return catalogue.is_none();
    };
    let listed = lookups
        .catalogues
        .get(catalogue)
        .is_some_and(|events| events.contains(uuid));
    listed != catalogue.is_none()
}

fn compare(left: &Value, op: Op, right: &Value) -> bool {
    match op {
        Op::Eq => equal(left, right),
        Op::Ne => !equal(left, right),
        Op::Lt => order(left, right).is_some_and(Ordering::is_lt),
        Op::Le => order(left, right).is_some_and(Ordering::is_le),
        Op::Gt => order(left, right).is_some_and(Ordering::is_gt),
        Op::Ge => order(left, right).is_some_and(Ordering::is_ge),
    }
}

fn equal(left: &Value, right: &Value) -> bool {
    order(left, right).map_or(left == right, Ordering::is_eq)
}

/// Numbers compare as numbers (as integers when both are, so nanosecond times stay exact),
/// text and booleans among themselves; anything else has no order. Text that reads as a time
/// compares with an integer as nanoseconds: date attributes are stored as text, and a rule
/// names times in nanoseconds.
fn order(left: &Value, right: &Value) -> Option<Ordering> {
    match (left, right) {
        (Value::String(text), Value::Number(nanos)) => Some(time(text)?.cmp(&nanos.as_i64()?)),
        (Value::Number(nanos), Value::String(text)) => Some(nanos.as_i64()?.cmp(&time(text)?)),
        (Value::Number(a), Value::Number(b)) => match (a.as_i64(), b.as_i64()) {
            (Some(a), Some(b)) => Some(a.cmp(&b)),
            _ => a.as_f64()?.partial_cmp(&b.as_f64()?),
        },
        (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
        (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
        _ => None,
    }
}

fn time(text: &str) -> Option<i64> {
    Timestamp::parse(text)?.unix_nanos()
}
