//! Smart catalogue rules: the JSON format, its validation, and the interval operators the
//! rules use. Evaluating a rule against a room lives in `model`, the only module that knows
//! the room layout.

pub mod intervals;
mod predicate;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub use intervals::{Item, Source};
pub use predicate::{Lookups, test};

/// The fixed event fields a predicate can name; any other field is an attribute, `#key`.
pub const FIELDS: [&str; 13] = [
    "uuid",
    "start",
    "stop",
    "author",
    "created",
    "origin",
    "status",
    "confidence",
    "start_err",
    "stop_err",
    "source_id",
    "tags",
    "products",
];

/// A set of intervals. Each form is told apart by its own key, so the enum is untagged;
/// unknown keys are refused, so a misspelt option is never silently ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Set {
    All {
        all: bool,
    },
    Catalogue {
        catalogue: String,
    },
    Filter {
        filter: Box<Set>,
        #[serde(rename = "where")]
        predicate: Predicate,
    },
    Merge {
        merge: Box<Set>,
        #[serde(default)]
        gap: f64,
    },
    Intersect {
        intersect: (Box<Set>, Box<Set>),
    },
    Subtract {
        subtract: (Box<Set>, Box<Set>),
    },
    Union {
        union: Vec<Set>,
    },
    Shift {
        shift: Box<Set>,
        by: f64,
    },
    Tile {
        tile: Box<Set>,
        width: f64,
    },
    Window {
        window: (Box<Set>, Box<Set>),
        within: f64,
    },
    Closest {
        closest: (Box<Set>, Box<Set>),
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Predicate {
    Cmp(String, Op, Value),
    Match(String, String),
    Has(String),
    Contains(String, Value),
    Not(Box<Predicate>),
    And(Vec<Predicate>),
    Or(Vec<Predicate>),
    /// `None`: listed by no catalogue.
    InCatalogue(Option<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    #[serde(rename = "==")]
    Eq,
    #[serde(rename = "!=")]
    Ne,
    #[serde(rename = "<")]
    Lt,
    #[serde(rename = "<=")]
    Le,
    #[serde(rename = ">")]
    Gt,
    #[serde(rename = ">=")]
    Ge,
}

#[derive(Debug, Error)]
pub enum RuleError {
    #[error("not a rule: {0}")]
    Format(#[source] serde_json::Error),
    #[error("\"all\" must be true")]
    All,
    #[error("{0} is neither an event field nor an attribute (#key)")]
    Field(String),
    #[error("{pattern} is not a regular expression")]
    Regex {
        pattern: String,
        #[source]
        source: regex::Error,
    },
    #[error("{key} {value} is not a length in seconds this rule takes")]
    Length { key: &'static str, value: f64 },
    #[error("the rule reaches catalogue {0} again through itself")]
    Cycle(String),
    #[error("tiling makes more than {} intervals", intervals::MAX_TILES)]
    TooManyTiles,
}

/// Reads and validates a rule.
pub fn parse(rule: &Value) -> Result<Set, RuleError> {
    let set = Set::deserialize(rule).map_err(RuleError::Format)?;
    validate(&set)?;
    Ok(set)
}

fn validate(set: &Set) -> Result<(), RuleError> {
    match set {
        Set::All { all } => all.then_some(()).ok_or(RuleError::All),
        Set::Catalogue { .. } => Ok(()),
        Set::Filter { filter, predicate } => {
            validate(filter)?;
            validate_predicate(predicate)
        }
        Set::Merge { merge, gap } => {
            length("gap", *gap, true)?;
            validate(merge)
        }
        Set::Shift { shift, by } => {
            length("by", by.abs(), true)?;
            validate(shift)
        }
        Set::Tile { tile, width } => {
            length("width", *width, false)?;
            validate(tile)
        }
        Set::Window {
            window: (a, b),
            within,
        } => {
            length("within", *within, true)?;
            validate(a)?;
            validate(b)
        }
        Set::Intersect { intersect: (a, b) }
        | Set::Subtract { subtract: (a, b) }
        | Set::Closest { closest: (a, b) } => {
            validate(a)?;
            validate(b)
        }
        Set::Union { union } => union.iter().try_for_each(validate),
    }
}

fn validate_predicate(predicate: &Predicate) -> Result<(), RuleError> {
    match predicate {
        Predicate::Cmp(name, _, _) | Predicate::Has(name) | Predicate::Contains(name, _) => {
            field(name)
        }
        Predicate::Match(name, pattern) => {
            field(name)?;
            regex(pattern).map(|_| ())
        }
        Predicate::Not(inner) => validate_predicate(inner),
        Predicate::And(all) | Predicate::Or(all) => all.iter().try_for_each(validate_predicate),
        Predicate::InCatalogue(_) => Ok(()),
    }
}

pub(crate) fn regex(pattern: &str) -> Result<regex::Regex, RuleError> {
    regex::Regex::new(pattern).map_err(|source| RuleError::Regex {
        pattern: pattern.to_owned(),
        source,
    })
}

fn field(name: &str) -> Result<(), RuleError> {
    let attribute = name.strip_prefix('#').is_some_and(|key| !key.is_empty());
    if attribute || FIELDS.contains(&name) {
        Ok(())
    } else {
        Err(RuleError::Field(name.to_owned()))
    }
}

/// A length in seconds must be finite and at least zero, or above zero when `empty` is
/// refused (a tile of width zero never ends).
fn length(key: &'static str, value: f64, empty: bool) -> Result<(), RuleError> {
    let valid = value.is_finite() && (value > 0.0 || (empty && value == 0.0));
    valid.then_some(()).ok_or(RuleError::Length { key, value })
}
