//! The filter box's search: what its text means, the rows it keeps, and the builder that
//! writes search text from field / operator / value rows.

use cocat_core::query::{self, Kind};
use cocat_core::rules::Predicate;

/// What the filter box holds.
#[derive(Debug, Clone, PartialEq)]
pub enum Query {
    Empty,
    /// Only plain words: the row filter of old, which also reads a whole start time.
    Words,
    Rule(Predicate),
    /// Text that does not read, and why, in plain words.
    Broken(String),
}

/// Reads the box's text; `catalogues` are the project's (uuid, name) pairs.
pub fn read(text: &str, catalogues: &[(String, String)]) -> Query {
    if text.trim().is_empty() {
        return Query::Empty;
    }
    match query::parse(text, catalogues) {
        Ok(predicate) if plain_words(&predicate) => Query::Words,
        Ok(predicate) => Query::Rule(predicate),
        Err(error) => Query::Broken(sentence(&error.to_string())),
    }
}

fn sentence(text: &str) -> String {
    let mut characters = text.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(characters).collect()
    })
}

fn plain_words(predicate: &Predicate) -> bool {
    match predicate {
        Predicate::Text(_) => true,
        Predicate::And(all) => all.iter().all(plain_words),
        _ => false,
    }
}

/// What a builder row can say about its field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
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

/// The choices that fit a kind of field, the first one the default.
pub fn choices(kind: Kind) -> &'static [Choice] {
    use Choice::*;
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

/// The words a choice reads as for a kind of field.
pub fn label(kind: Kind, choice: Choice) -> &'static str {
    use Choice::*;
    match (kind, choice) {
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

pub fn needs_value(choice: Choice) -> bool {
    !matches!(choice, Choice::Set | Choice::NotSet)
}

/// One builder row as search text: `confidence:>=0.8`, `NOT tag:shock`, `created:now-7d`.
pub fn clause(field: &str, choice: Choice, value: &str) -> String {
    use Choice::*;
    let value = value.trim();
    let quoted = quote(value);
    match choice {
        Contains => format!("{field}:{}", text_value(value)),
        Is if is_number(value) => format!("{field}:{value}"),
        Is => format!("{field}:{quoted}"),
        IsNot => format!("NOT {field}:{quoted}"),
        Matches => format!("{field}:/{value}/"),
        Greater => format!("{field}:>{value}"),
        AtLeast => format!("{field}:>={value}"),
        Less => format!("{field}:<{value}"),
        AtMost => format!("{field}:<={value}"),
        On => format!("{field}:{value}"),
        After => format!("{field}:>{value}"),
        Before => format!("{field}:<{value}"),
        InLastDays => format!("{field}:now-{value}d"),
        Set => format!("_exists_:{field}"),
        NotSet => format!("NOT _exists_:{field}"),
    }
}

/// The rows' clauses as one search: all of them, or any of them.
pub fn search_text(clauses: &[String], any: bool) -> String {
    let clauses: Vec<&String> = clauses.iter().filter(|clause| !clause.is_empty()).collect();
    match (any, clauses.len()) {
        (true, 2..) => clauses
            .iter()
            .map(|clause| format!("({clause})"))
            .collect::<Vec<_>>()
            .join(" OR "),
        _ => clauses
            .iter()
            .map(|clause| clause.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// A value as a word the search reads back as itself.
fn quote(value: &str) -> String {
    let plain = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '@'));
    match plain {
        true => value.to_owned(),
        false => format!("\"{}\"", value.replace('"', "")),
    }
}

/// Contains on text: a number must be quoted, or an attribute compares it as a number.
fn text_value(value: &str) -> String {
    match is_number(value) {
        true => format!("\"{value}\""),
        false => quote(value),
    }
}

fn is_number(value: &str) -> bool {
    value.parse::<f64>().is_ok()
}

/// A preset smart catalogue: its name and search. `me` is the visitor's email.
pub fn presets(me: &str) -> Vec<(&'static str, String)> {
    vec![
        ("Recently added", "created:now-7d".to_owned()),
        ("Mine", format!("author:={}", quote(me))),
        (
            "Not reviewed yet",
            "NOT status:reviewed NOT status:final".to_owned(),
        ),
        ("Low confidence", "confidence:<0.5".to_owned()),
        ("With a shape", "_exists_:shape".to_owned()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<(String, String)> {
        vec![("c1".into(), "Shocks".into())]
    }

    fn reads(text: &str) {
        assert!(
            query::parse(text, &names()).is_ok(),
            "“{text}” does not read: {:?}",
            query::parse(text, &names())
        );
    }

    #[test]
    fn plain_words_keep_the_old_filter() {
        assert_eq!(read("  ", &names()), Query::Empty);
        assert_eq!(read("shock alice", &names()), Query::Words);
        assert!(matches!(read("tag:shock", &names()), Query::Rule(_)));
        assert!(matches!(read("shock OR alice", &names()), Query::Rule(_)));
    }

    #[test]
    fn broken_text_says_why() {
        let Query::Broken(why) = read("tga:shock", &names()) else {
            panic!("a typo reads");
        };
        assert!(why.starts_with("There is no field"), "{why}");
        assert!(why.contains("tag"), "{why}");
    }

    #[test]
    fn every_choice_of_every_kind_writes_text_that_reads() {
        let samples: [(&str, Kind, &str); 9] = [
            ("author", Kind::Text, "alice smith"),
            ("confidence", Kind::Number, "0.8"),
            ("#density", Kind::Attribute, "12"),
            ("created", Kind::Time, "2015-03-01"),
            ("duration", Kind::Duration, "10min"),
            ("status", Kind::Enum(&query::STATUSES), "reviewed"),
            ("tag", Kind::Tags, "shock"),
            ("catalogue", Kind::Catalogue, "Shocks"),
            ("shape", Kind::Presence, ""),
        ];
        for (field, kind, value) in samples {
            for &choice in choices(kind) {
                let value = match choice {
                    Choice::InLastDays => "7",
                    Choice::Matches => "^al",
                    _ => value,
                };
                reads(&clause(field, choice, value));
            }
        }
    }

    #[test]
    fn rows_join_as_all_or_any() {
        let rows = vec!["tag:shock".to_owned(), "NOT status:final".to_owned()];
        assert_eq!(search_text(&rows, false), "tag:shock NOT status:final");
        assert_eq!(
            search_text(&rows, true),
            "(tag:shock) OR (NOT status:final)"
        );
        reads(&search_text(&rows, true));
        assert_eq!(search_text(&rows[..1], true), "tag:shock");
    }

    #[test]
    fn values_are_quoted_when_they_must_be() {
        assert_eq!(clause("author", Choice::Contains, "alice"), "author:alice");
        assert_eq!(
            clause("catalogue", Choice::Is, "Shocks 2019"),
            "catalogue:\"Shocks 2019\""
        );
        assert_eq!(clause("#n", Choice::Contains, "3"), "#n:\"3\"");
        assert_eq!(clause("#n", Choice::Is, "3"), "#n:3");
        assert_eq!(clause("created", Choice::InLastDays, "7"), "created:now-7d");
    }

    #[test]
    fn presets_read() {
        for (_, text) in presets("bob.durand@example.org") {
            reads(&text);
        }
    }
}
