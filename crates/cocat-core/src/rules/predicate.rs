//! Testing a predicate against one item. The caller reads the item's fields, so this module
//! knows nothing of the room layout.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use regex::Regex;
use serde_json::Value;

use super::{Op, Predicate, RuleError};
use crate::time::Timestamp;

/// What a predicate needs besides the item: its regexes, compiled once, and the events of
/// each catalogue it names (for `None`: the events some catalogue lists).
#[derive(Debug, Default)]
pub struct Lookups {
    regexes: HashMap<String, Regex>,
    catalogues: HashMap<Option<String>, HashSet<String>>,
}

impl Lookups {
    pub fn new(
        predicate: &Predicate,
        events_of: &mut impl FnMut(Option<&str>) -> Result<HashSet<String>, RuleError>,
    ) -> Result<Self, RuleError> {
        let mut lookups = Self::default();
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
}

/// Whether an item satisfies `predicate`: `event` is its uuid when it is an event, `field`
/// reads one of its fields (`None` when unset; a null value counts as unset). As in tscat,
/// `== null` asks whether a field is unset and `!= null` whether it is set; any other
/// comparison with an unset field is false, whatever the operator.
pub fn test(
    predicate: &Predicate,
    event: Option<&str>,
    field: &impl Fn(&str) -> Option<Value>,
    lookups: &Lookups,
) -> bool {
    let value = |name: &str| field(name).filter(|value| !value.is_null());
    match predicate {
        Predicate::Cmp(name, op, Value::Null) => match op {
            Op::Eq => value(name).is_none(),
            Op::Ne => value(name).is_some(),
            _ => false,
        },
        Predicate::Cmp(name, op, right) => {
            value(name).is_some_and(|left| compare(&left, *op, right))
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
    }
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
