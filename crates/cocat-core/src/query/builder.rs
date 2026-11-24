//! The search builder: rows of field · test · value, the words both apps show for them, the
//! text each row writes, and the rows a flat search reads back into. The web app and SciQLop
//! both build searches from these, so a search made in one opens unchanged in the other.

use serde_json::Value;

use super::print::{contained, duration_text, span_of, value_text};
use super::{Kind, canonical, kind_of, named};
use crate::rules::{self, Op, Predicate, Set, relative};
use crate::time::Timestamp;

const DAY: i64 = 86_400_000_000_000;

/// What a row says about its field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Test {
    Contains,
    Is,
    IsNot,
    Matches,
    Greater,
    AtLeast,
    Less,
    AtMost,
    On,
    After,
    Before,
    InLastDays,
    Set,
    NotSet,
}

const TESTS: [Test; 14] = {
    use Test::*;
    [
        Contains, Is, IsNot, Matches, Greater, AtLeast, Less, AtMost, On, After, Before,
        InLastDays, Set, NotSet,
    ]
};

impl Test {
    /// The name scripts use for it.
    pub fn name(self) -> &'static str {
        use Test::*;
        match self {
            Contains => "contains",
            Is => "is",
            IsNot => "is_not",
            Matches => "matches",
            Greater => "more_than",
            AtLeast => "at_least",
            Less => "less_than",
            AtMost => "at_most",
            On => "on",
            After => "after",
            Before => "before",
            InLastDays => "in_last_days",
            Set => "is_set",
            NotSet => "is_not_set",
        }
    }

    pub fn named(name: &str) -> Option<Test> {
        TESTS.into_iter().find(|test| test.name() == name)
    }

    pub fn needs_value(self) -> bool {
        !matches!(self, Test::Set | Test::NotSet)
    }

    fn negated(self) -> Option<Test> {
        match self {
            Test::Is => Some(Test::IsNot),
            Test::Set => Some(Test::NotSet),
            _ => None,
        }
    }
}

/// The tests that fit a kind of field, the first one the default.
pub fn tests(kind: Kind) -> &'static [Test] {
    use Test::*;
    match kind {
        Kind::Text => &[Contains, Is, Matches, Set, NotSet],
        Kind::Number => &[AtLeast, AtMost, Greater, Less, Is, Set, NotSet],
        Kind::Attribute => &[Is, Contains, AtLeast, AtMost, Greater, Less, Set, NotSet],
        Kind::Time => &[InLastDays, On, After, Before, Set, NotSet],
        Kind::Duration => &[Greater, Less],
        Kind::Enum(_) | Kind::Tags | Kind::Catalogue => &[Is, IsNot],
        Kind::Presence => &[Set, NotSet],
    }
}

/// The words a test reads as for a kind of field.
pub fn label(kind: Kind, test: Test) -> &'static str {
    use Test::*;
    match (kind, test) {
        (Kind::Duration, Greater) => "longer than",
        (Kind::Duration, Less) => "shorter than",
        (Kind::Tags, Is) => "has",
        (Kind::Tags, IsNot) => "does not have",
        (Kind::Catalogue, Is) => "is in",
        (Kind::Catalogue, IsNot) => "is not in",
        (Kind::Presence, Set) => "has one",
        (Kind::Presence, NotSet) => "has none",
        (_, Contains) => "contains",
        (_, Is) => "is",
        (_, IsNot) => "is not",
        (_, Matches) => "matches the pattern",
        (_, Greater) => "is more than",
        (_, AtLeast) => "is at least",
        (_, Less) => "is less than",
        (_, AtMost) => "is at most",
        (_, On) => "is on",
        (_, After) => "is after",
        (_, Before) => "is before",
        (_, InLastDays) => "is in the last … days",
        (_, Set) => "is set",
        (_, NotSet) => "is not set",
    }
}

/// How a field reads in a field menu; an attribute `#key` reads as "key (attribute)".
pub fn field_label(field: &str) -> String {
    let fixed = match field {
        "start" => "Start",
        "stop" => "Stop",
        "duration" => "Duration",
        "tag" => "Tag",
        "status" => "Status",
        "confidence" => "Confidence",
        "author" => "Author",
        "catalogue" => "Catalogue",
        "created" => "Added",
        "origin" => "Made by",
        "source_id" => "Source id",
        "product" => "Product",
        "start_err" => "Start uncertainty (s)",
        "stop_err" => "Stop uncertainty (s)",
        "uuid" => "Event id",
        "shape" => "Shape",
        other => {
            return match other.strip_prefix('#') {
                Some(key) => format!("{key} (attribute)"),
                None => other.to_owned(),
            };
        }
    };
    fixed.to_owned()
}

/// One row as search text: `confidence:>=0.8`, `NOT tag:shock`, `created:now-7d`. A value
/// that does not fit its test (a word for a number) writes text that does not read, and the
/// search says why.
pub fn clause(field: &str, test: Test, value: &str) -> String {
    use Test::*;
    let kind = kind_of(field).unwrap_or(Kind::Text);
    let value = value.trim();
    match test {
        Contains => format!("{field}:{}", value_text(value)),
        Is => format!("{field}:{}", equal(kind, value)),
        IsNot => format!("NOT {field}:{}", equal(kind, value)),
        Matches => format!("{field}:/{}/", value.replace('/', "\\/")),
        Greater | After => format!("{field}:>{value}"),
        AtLeast => format!("{field}:>={value}"),
        Less | Before => format!("{field}:<{value}"),
        AtMost => format!("{field}:<={value}"),
        On => format!("{field}:{value}"),
        InLastDays => format!("{field}:now-{value}d"),
        Set => format!("_exists_:{field}"),
        NotSet => format!("NOT _exists_:{field}"),
    }
}

/// The value of "is": text and attributes compare exactly (`=`), since a bare word on them
/// means "contains".
fn equal(kind: Kind, value: &str) -> String {
    let number = value.parse::<f64>().is_ok();
    match kind {
        Kind::Number => value.to_owned(),
        Kind::Attribute if number => value.to_owned(),
        Kind::Text | Kind::Attribute => format!("={}", value_text(value)),
        _ => value_text(value),
    }
}

/// A value as a word the search reads back as itself: quoted when it must be.
pub fn value(text: &str) -> String {
    value_text(text)
}

/// The rows' clauses as one search: all of them, or any of them. Empty clauses (rows not
/// filled in yet) are left out.
pub fn search_text(clauses: &[String], any: bool) -> String {
    let clauses: Vec<&str> = clauses
        .iter()
        .map(String::as_str)
        .filter(|clause| !clause.is_empty())
        .collect();
    match (any, clauses.len()) {
        (true, 2..) => clauses
            .iter()
            .map(|clause| format!("({clause})"))
            .collect::<Vec<_>>()
            .join(" OR "),
        _ => clauses.join(" "),
    }
}

/// A builder row. `field` is a query name (`tag`, `#density`); `value` is empty for a test
/// that takes none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub field: String,
    pub test: Test,
    pub value: String,
}

impl Row {
    fn new(field: &str, test: Test, value: impl Into<String>) -> Row {
        Row {
            field: field.to_owned(),
            test,
            value: value.into(),
        }
    }

    pub fn text(&self) -> String {
        clause(&self.field, self.test, &self.value)
    }
}

/// The rows of a rule's search, whatever it searches (all events, a catalogue, the public
/// catalogues), or `None` when the rule is not a search or its search is not flat.
pub fn rule_rows(rule: &Value, catalogues: &[(String, String)]) -> Option<(bool, Vec<Row>)> {
    let Ok(Set::Filter { filter, predicate }) = rules::parse(rule) else {
        return None;
    };
    let searched = matches!(
        *filter,
        Set::All { .. } | Set::Catalogue { .. } | Set::Public { .. }
    );
    searched.then(|| rows(&predicate, catalogues)).flatten()
}

/// The rows a predicate reads back into, and whether they join with "any": `None` unless it
/// is flat, one level of AND or of OR whose parts are each a row. The rows always write a
/// search that reads back into the same predicate.
pub fn rows(predicate: &Predicate, catalogues: &[(String, String)]) -> Option<(bool, Vec<Row>)> {
    let predicate = canonical(predicate.clone());
    let reader = Reader { catalogues };
    let (any, found) = match &predicate {
        Predicate::Or(clauses) => (
            true,
            clauses
                .iter()
                .map(|c| reader.part(c))
                .collect::<Option<Vec<_>>>()?,
        ),
        Predicate::And(clauses) => (false, reader.all(clauses)?),
        single => (false, vec![reader.row(single)?]),
    };
    let clauses: Vec<String> = found.iter().map(Row::text).collect();
    let back = super::parse(&search_text(&clauses, any), catalogues).ok()?;
    (back == predicate).then_some((any, found))
}

struct Reader<'c> {
    catalogues: &'c [(String, String)],
}

impl Reader<'_> {
    /// AND's clauses as rows, a day's two bounds as one "is on".
    fn all(&self, clauses: &[Predicate]) -> Option<Vec<Row>> {
        let mut found = Vec::new();
        let mut index = 0;
        while index < clauses.len() {
            let on = clauses
                .get(index + 1)
                .and_then(|next| self.on(&clauses[index], next));
            match on {
                Some(row) => {
                    found.push(row);
                    index += 2;
                }
                None => {
                    found.push(self.row(&clauses[index])?);
                    index += 1;
                }
            }
        }
        Some(found)
    }

    /// One part of an OR: a row, or the two bounds of "is on".
    fn part(&self, clause: &Predicate) -> Option<Row> {
        match clause {
            Predicate::And(bounds) if bounds.len() == 2 => self.on(&bounds[0], &bounds[1]),
            _ => self.row(clause),
        }
    }

    fn on(&self, first: &Predicate, second: &Predicate) -> Option<Row> {
        let (name, text) = span_of(first, second)?;
        Some(Row::new(name, Test::On, text))
    }

    fn row(&self, predicate: &Predicate) -> Option<Row> {
        let found = self.any_row(predicate)?;
        let kind = kind_of(&found.field)?;
        tests(kind).contains(&found.test).then_some(found)
    }

    /// The row a predicate reads as, whether or not its field offers that test.
    fn any_row(&self, predicate: &Predicate) -> Option<Row> {
        Some(match predicate {
            Predicate::Not(inner) => {
                let found = self.any_row(inner)?;
                Row {
                    test: found.test.negated()?,
                    ..found
                }
            }
            Predicate::Has(field) => Row::new(named(field)?.0, Test::Set, ""),
            Predicate::InCatalogue(Some(uuid)) => {
                Row::new("catalogue", Test::Is, self.catalogue(uuid))
            }
            Predicate::Contains(field, Value::String(word)) => {
                Row::new(named(field)?.0, Test::Is, word.as_str())
            }
            Predicate::Match(field, pattern) => {
                let name = named(field)?.0;
                match contained(pattern) {
                    Some(text) => Row::new(name, Test::Contains, text),
                    None => Row::new(name, Test::Matches, pattern.as_str()),
                }
            }
            Predicate::Cmp(field, op, value) => compared(field, *op, value)?,
            _ => return None,
        })
    }

    /// A catalogue's name, or its uuid when no single catalogue has that name.
    fn catalogue(&self, uuid: &str) -> String {
        let name = self
            .catalogues
            .iter()
            .find(|(known, _)| known == uuid)
            .map(|(_, name)| name);
        let unique = name.filter(|name| {
            self.catalogues
                .iter()
                .filter(|(_, other)| other == *name)
                .count()
                == 1
        });
        unique.map_or_else(|| uuid.to_owned(), Clone::clone)
    }
}

fn compared(field: &str, op: Op, value: &Value) -> Option<Row> {
    let (name, kind) = named(field)?;
    let (test, text) = match kind {
        Kind::Time => time_row(op, value)?,
        Kind::Duration => {
            let test = match op {
                Op::Gt => Test::Greater,
                Op::Lt => Test::Less,
                _ => return None,
            };
            (test, duration_text(value.as_f64()?))
        }
        _ => {
            let test = match op {
                Op::Eq => Test::Is,
                Op::Gt => Test::Greater,
                Op::Ge => Test::AtLeast,
                Op::Lt => Test::Less,
                Op::Le => Test::AtMost,
                Op::Ne => return None,
            };
            let text = match value {
                Value::String(text) => text.clone(),
                Value::Number(number) => number.to_string(),
                _ => return None,
            };
            (test, text)
        }
    };
    Some(Row::new(name, test, text))
}

/// "is in the last N days" from `>= now-Nd`; "is after" from `>` an instant or `>=` the
/// midnight after a day; "is before" from `<`.
fn time_row(op: Op, value: &Value) -> Option<(Test, String)> {
    if let Some(offset) = relative::offset_of(value) {
        let days = offset.strip_prefix('-')?.strip_suffix('d')?;
        let whole = !days.is_empty() && days.chars().all(|c| c.is_ascii_digit());
        return (op == Op::Ge && whole).then(|| (Test::InLastDays, days.to_owned()));
    }
    let nanos = value.as_i64()?;
    let instant = |nanos| Timestamp::from_unix_nanos(nanos)?.rfc3339().ok();
    let date = |nanos| {
        let text = instant(nanos)?;
        text.strip_suffix("T00:00:00Z").map(str::to_owned)
    };
    match op {
        Op::Gt => Some((Test::After, instant(nanos)?)),
        Op::Ge if date(nanos).is_some() => Some((Test::After, date(nanos.checked_sub(DAY)?)?)),
        Op::Lt => Some((Test::Before, date(nanos).or_else(|| instant(nanos))?)),
        _ => None,
    }
}
