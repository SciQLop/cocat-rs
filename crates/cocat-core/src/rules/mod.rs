//! Smart catalogue rules: the JSON format, its validation, and the interval operators the
//! rules use. Evaluating a rule against a room lives in `model`, the only module that knows
//! the room layout.

pub mod intervals;
mod parse;
mod predicate;
pub mod relative;
mod tscat;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub use intervals::{Item, Source};
pub use parse::parse;
pub use predicate::{ANY_TEXT, Lookups, test};
pub use tscat::{TscatError, rule_from_tscat};

/// The event fields a predicate can name; any other field is an attribute, `#key`.
/// `duration` is computed: stop − start, in seconds.
pub const FIELDS: [&str; 15] = [
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
    "duration",
    "shape",
];

/// A set of intervals. Each form is told apart by its own key (see `parse`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Set {
    All {
        all: bool,
    },
    Catalogue {
        catalogue: String,
    },
    /// The events of every public catalogue on the server. A room does not hold them, so it
    /// evaluates this as nothing; the clients that show them ask the server
    /// (`GET /api/public/events`).
    Public {
        public: bool,
    },
    Filter {
        filter: Box<Set>,
        #[serde(rename = "where")]
        predicate: Predicate,
    },
    Merge {
        merge: Box<Set>,
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

impl Set {
    /// Whether it takes events from the public catalogues, which a room does not hold.
    pub fn reaches_public(&self) -> bool {
        match self {
            Set::Public { .. } => true,
            Set::All { .. } | Set::Catalogue { .. } => false,
            Set::Filter { filter: one, .. }
            | Set::Merge { merge: one, .. }
            | Set::Shift { shift: one, .. }
            | Set::Tile { tile: one, .. } => one.reaches_public(),
            Set::Intersect { intersect: (a, b) }
            | Set::Subtract { subtract: (a, b) }
            | Set::Window { window: (a, b), .. }
            | Set::Closest { closest: (a, b) } => a.reaches_public() || b.reaches_public(),
            Set::Union { union } => union.iter().any(Set::reaches_public),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
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
    /// The word is part of some text of the item, or begins its start time.
    Text(String),
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
    /// `at` is where in the rule, as a path: `rule.union[1].where.cmp`.
    #[error("{at}: {problem}")]
    Invalid { at: String, problem: Problem },
    #[error("the rule reaches catalogue {0} again through itself")]
    Cycle(String),
    #[error("tiling makes more than {} intervals", intervals::MAX_TILES)]
    TooManyTiles,
}

/// What is wrong with a part of a rule.
#[derive(Debug, Error)]
pub enum Problem {
    #[error("{0}")]
    Format(String),
    #[error("\"all\" must be true")]
    All,
    #[error("\"public\" must be true")]
    Public,
    #[error("{0} is neither an event field nor an attribute (#key)")]
    Field(String),
    #[error("{pattern} is not a regular expression")]
    Regex {
        pattern: String,
        #[source]
        source: regex::Error,
    },
    #[error("{0} is not a length in seconds this rule takes")]
    Length(f64),
}

pub(crate) fn regex(pattern: &str) -> Result<regex::Regex, Problem> {
    regex::Regex::new(pattern).map_err(|source| Problem::Regex {
        pattern: pattern.to_owned(),
        source,
    })
}
