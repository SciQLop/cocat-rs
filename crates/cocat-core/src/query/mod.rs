//! The search language: one line of text, read into a rule `Predicate` and printed back from
//! one. `tag:shock confidence:>=0.8 NOT status:rejected` is a search; saved, it is the
//! smart catalogue `{"filter": {"all": true}, "where": <predicate>}`. The design is in
//! `docs/superpowers/specs/2026-11-09-search-smart-catalogues-design.md`.
//!
//! Catalogues are named in a query and kept by uuid in a rule, so reading and printing take
//! the room's catalogues as `(uuid, name)` pairs.

pub mod builder;
mod grammar;
mod meaning;
mod print;
pub mod search_box;

use serde_json::{Value, json};
use thiserror::Error;

use crate::rules::{self, Predicate, Set};

pub use print::print;

/// A query that does not read: where (a character index from 0), how many characters the
/// wrong part spans when known, what is wrong, and the field that was likely meant.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message} (at character {})", .position + 1)]
pub struct QueryError {
    pub position: usize,
    pub length: Option<usize>,
    pub message: String,
    pub suggestion: Option<String>,
}

impl QueryError {
    /// The error at byte `at` of `text`.
    fn at(text: &str, at: usize, message: String) -> Self {
        let at = at.min(text.len());
        Self {
            position: text[..at].chars().count(),
            length: None,
            message,
            suggestion: None,
        }
    }

    /// The error on bytes `range` of `text`.
    fn spanning(text: &str, range: &std::ops::Range<usize>, message: String) -> Self {
        Self {
            length: text.get(range.clone()).map(|part| part.chars().count()),
            ..Self::at(text, range.start, message)
        }
    }
}

/// What a field holds, which says how a query matches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Contains, ignoring case; `=` for the exact text, `/regex/`.
    Text,
    Number,
    /// A year, month or day is the span it covers; also full ISO times and `now-7d`.
    Time,
    /// Seconds, or with a unit: s, min, h, d.
    Duration,
    /// One of a fixed set of values.
    Enum(&'static [&'static str]),
    /// A list of words, each matched exactly.
    Tags,
    /// A catalogue, by name.
    Catalogue,
    /// An attribute, `#name`: a number when the value reads as one, else text.
    Attribute,
    /// Only `_exists_:` tests it.
    Presence,
}

/// Every kind of field, in no particular order.
pub const KINDS: [Kind; 9] = [
    Kind::Text,
    Kind::Number,
    Kind::Time,
    Kind::Duration,
    Kind::Enum(&STATUSES),
    Kind::Tags,
    Kind::Catalogue,
    Kind::Attribute,
    Kind::Presence,
];

impl Kind {
    /// The name scripts use for it; the one set of fixed values, the statuses, is `choice`.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Number => "number",
            Kind::Time => "time",
            Kind::Duration => "duration",
            Kind::Enum(_) => "choice",
            Kind::Tags => "tags",
            Kind::Catalogue => "catalogue",
            Kind::Attribute => "attribute",
            Kind::Presence => "presence",
        }
    }

    pub fn named(name: &str) -> Option<Kind> {
        KINDS.into_iter().find(|kind| kind.name() == name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    /// The name a query uses.
    pub name: &'static str,
    /// The field the rule names.
    pub rule: &'static str,
    pub kind: Kind,
}

/// The statuses a search can ask for; deleted events are in the trash, never found.
pub const STATUSES: [&str; 5] = ["preliminary", "confirmed", "reviewed", "final", "rejected"];

const fn field(name: &'static str, rule: &'static str, kind: Kind) -> Field {
    Field { name, rule, kind }
}

/// The fields a query can name, in the order a field menu lists them. Attributes, `#name`,
/// come on top of these.
pub const FIELDS: [Field; 16] = [
    field("start", "start", Kind::Time),
    field("stop", "stop", Kind::Time),
    field("duration", "duration", Kind::Duration),
    field("tag", "tags", Kind::Tags),
    field("status", "status", Kind::Enum(&STATUSES)),
    field("confidence", "confidence", Kind::Number),
    field("author", "author", Kind::Text),
    field("catalogue", "catalogue", Kind::Catalogue),
    field("created", "created", Kind::Time),
    field("origin", "origin", Kind::Text),
    field("source_id", "source_id", Kind::Text),
    field("product", "products", Kind::Tags),
    field("start_err", "start_err", Kind::Number),
    field("stop_err", "stop_err", Kind::Number),
    field("uuid", "uuid", Kind::Text),
    field("shape", "shape", Kind::Presence),
];

/// Other names a query accepts for a field.
const ALIASES: [(&str, &str); 2] = [("tags", "tag"), ("products", "product")];

/// The kind of the field a query name stands for, attributes (`#name`) included.
pub fn kind_of(name: &str) -> Option<Kind> {
    resolve(name).map(|(_, kind)| kind)
}

/// The rule field and kind of a query name: a fixed field, an alias, or an attribute.
fn resolve(name: &str) -> Option<(String, Kind)> {
    let name = ALIASES
        .iter()
        .find(|(alias, _)| *alias == name)
        .map_or(name, |(_, field)| field);
    if let Some(field) = FIELDS.iter().find(|field| field.name == name) {
        return Some((field.rule.to_owned(), field.kind));
    }
    let attribute = name.strip_prefix('#').is_some_and(|key| !key.is_empty());
    attribute.then(|| (name.to_owned(), Kind::Attribute))
}

/// How a search names the attribute `key`: bare, or `#key` when a field has that name;
/// quoted when the name needs it.
pub fn attribute_name(key: &str) -> String {
    match resolve(key) {
        Some(_) => hashed(key),
        None if grammar::is_identifier(key) => key.to_owned(),
        None => quoted(key),
    }
}

/// How a search writes a rule field: an attribute as `#key`, quoted when the key needs it.
pub fn written_name(field: &str) -> String {
    match field.strip_prefix('#') {
        Some(key) => hashed(key),
        None => field.to_owned(),
    }
}

/// A name as a search writes it (`tag`, `"B jump"`, `#"B jump"`), without its quotes.
pub fn unquoted(name: &str) -> String {
    let (hash, body) = name
        .strip_prefix('#')
        .map_or(("", name), |body| ("#", body));
    let Some(inner) = body
        .strip_prefix('"')
        .and_then(|body| body.strip_suffix('"'))
    else {
        return name.to_owned();
    };
    let mut key = hash.to_owned();
    let mut characters = inner.chars();
    while let Some(character) = characters.next() {
        key.extend(match character {
            '\\' => characters.next(),
            _ => Some(character),
        });
    }
    key
}

fn hashed(key: &str) -> String {
    match grammar::is_bare_attribute(key) {
        true => format!("#{key}"),
        false => format!("#{}", quoted(key)),
    }
}

/// `"name"`, where a quote or a backslash is escaped.
fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The query name of a rule field and its kind.
fn named(rule: &str) -> Option<(&str, Kind)> {
    if let Some(field) = FIELDS.iter().find(|field| field.rule == rule) {
        return Some((field.name, field.kind));
    }
    rule.starts_with('#').then_some((rule, Kind::Attribute))
}

/// Reads a query into a predicate. An empty query matches every event. A name that is no
/// field is an attribute: one nobody has matches nothing.
pub fn parse(text: &str, catalogues: &[(String, String)]) -> Result<Predicate, QueryError> {
    read(text, catalogues, None)
}

/// `parse` knowing the project's `attributes` (keys, without `#`): a name that is neither a
/// field nor one of them is refused, naming the closest.
pub fn parse_with_attributes(
    text: &str,
    catalogues: &[(String, String)],
    attributes: &[String],
) -> Result<Predicate, QueryError> {
    read(text, catalogues, Some(attributes))
}

fn read(
    text: &str,
    catalogues: &[(String, String)],
    attributes: Option<&[String]>,
) -> Result<Predicate, QueryError> {
    let predicate = match grammar::tree(text)? {
        Some(tree) => meaning::predicate(&tree, text, catalogues, attributes)?,
        None => Predicate::And(Vec::new()),
    };
    Ok(canonical(predicate))
}

/// The query of a rule and the catalogue it searches (`None`: all events), or `None` when the
/// rule has no query form: only `{"filter": <all or a catalogue>, "where": ...}` has one.
pub fn rule_query(
    rule: &Value,
    catalogues: &[(String, String)],
) -> Option<(String, Option<String>)> {
    let Ok(Set::Filter { filter, predicate }) = rules::parse(rule) else {
        return None;
    };
    let source = match *filter {
        Set::All { .. } => None,
        Set::Catalogue { catalogue } => Some(catalogue),
        _ => return None,
    };
    Some((print(&predicate, catalogues)?, source))
}

/// A rule's query as one search of all events, its source catalogue written as a
/// `catalogue:` clause, or `None` when the rule has no query form.
pub fn rule_search(rule: &Value, catalogues: &[(String, String)]) -> Option<String> {
    let Ok(Set::Filter { filter, predicate }) = rules::parse(rule) else {
        return None;
    };
    let predicate = match *filter {
        Set::All { .. } => predicate,
        Set::Catalogue { catalogue } => {
            Predicate::And(vec![Predicate::InCatalogue(Some(catalogue)), predicate])
        }
        _ => return None,
    };
    print(&canonical(predicate), catalogues)
}

/// The search of a rule over the public catalogues (see [`public_rule`]), or `None` for any
/// other rule. `catalogues` are the public catalogues' (id, name).
pub fn public_search(rule: &Value, catalogues: &[(String, String)]) -> Option<String> {
    match rules::parse(rule) {
        Ok(Set::Filter { filter, predicate }) if matches!(*filter, Set::Public { .. }) => {
            print(&canonical(predicate), catalogues)
        }
        _ => None,
    }
}

/// The rule that searches the events of every public catalogue with `predicate`.
pub fn public_rule(predicate: &Predicate) -> Value {
    json!({"filter": {"public": true}, "where": predicate})
}

/// The rule that searches `source` (`None`: all events) with `predicate`.
pub fn rule(predicate: &Predicate, source: Option<&str>) -> Value {
    let filter = match source {
        Some(catalogue) => json!({"catalogue": catalogue}),
        None => json!({"all": true}),
    };
    json!({"filter": filter, "where": predicate})
}

/// One form for predicates that mean the same: nested ANDs and ORs flattened, a single
/// clause unwrapped, double negation dropped, a comparison with null as presence, whole
/// numbers as integers. Printing relies on it to tell whether a query reads back the same.
pub(crate) fn canonical(predicate: Predicate) -> Predicate {
    match predicate {
        Predicate::And(clauses) => joined(clauses, Predicate::And, |p| match p {
            Predicate::And(inner) => Ok(inner),
            other => Err(other),
        }),
        Predicate::Or(clauses) => joined(clauses, Predicate::Or, |p| match p {
            Predicate::Or(inner) => Ok(inner),
            other => Err(other),
        }),
        Predicate::Not(inner) => negated(canonical(*inner)),
        Predicate::Cmp(field, rules::Op::Eq, Value::Null) => {
            Predicate::Not(Box::new(Predicate::Has(field)))
        }
        Predicate::Cmp(field, rules::Op::Ne, Value::Null) => Predicate::Has(field),
        Predicate::Cmp(field, op, value) => Predicate::Cmp(field, op, whole(value)),
        other => other,
    }
}

fn negated(predicate: Predicate) -> Predicate {
    match predicate {
        Predicate::Not(inner) => *inner,
        other => Predicate::Not(Box::new(other)),
    }
}

/// The clauses, canonical, with those of the same joint spliced in.
fn joined(
    clauses: Vec<Predicate>,
    join: fn(Vec<Predicate>) -> Predicate,
    same: fn(Predicate) -> Result<Vec<Predicate>, Predicate>,
) -> Predicate {
    let mut flat = Vec::new();
    for clause in clauses.into_iter().map(canonical) {
        match same(clause) {
            Ok(inner) => flat.extend(inner),
            Err(other) => flat.push(other),
        }
    }
    match <[Predicate; 1]>::try_from(flat) {
        Ok([single]) => single,
        Err(flat) => join(flat),
    }
}

/// A float that holds a whole number of `i64` range, as that integer.
fn whole(value: Value) -> Value {
    const EXACT: f64 = 9_007_199_254_740_992.0; // 2^53
    match value.as_f64() {
        Some(number) if !value.is_i64() && number.fract() == 0.0 && number.abs() < EXACT => {
            Value::from(number as i64)
        }
        _ => value,
    }
}
