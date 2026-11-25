//! What a search box does with its text, in the web app and in SciQLop: whether it is a
//! search or plain words, its colours, its completions, the preset searches, and the plain
//! words filter.

use crate::query::builder::value as quote;
use crate::query::{self, attribute_name, written_name};
use std::collections::BTreeMap;

use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::format_description::BorrowedFormatItem;
use time::macros::format_description;

use crate::model::Status;
use crate::rules::Predicate;

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

/// `text` with a capital first letter.
pub fn sentence(text: &str) -> String {
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
        let Query::Broken(why) = read("tag:shock OR", &names()) else {
            panic!("a dangling OR reads");
        };
        assert!(!why.is_empty());
        assert!(
            matches!(read("tga:shock", &names()), Query::Rule(_)),
            "a name that is no field is an attribute, which nobody may have"
        );
    }

    #[test]
    fn presets_read() {
        for (_, text) in presets("bob.durand@example.org") {
            reads(&text);
        }
    }
}

/// What the project holds that a search can name, for the completions.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Vocabulary {
    pub tags: Vec<String>,
    pub authors: Vec<String>,
    pub catalogues: Vec<String>,
    pub attributes: Vec<String>,
    /// The values a text attribute takes, by key, the most frequent first.
    pub attribute_values: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    /// What replaces the word being typed.
    pub insert: String,
    /// What kind of thing it is: "field", "tag", "catalogue"…
    pub kind: &'static str,
}

impl Suggestion {
    /// What it is, in a word a person reads beside it: `field`, `attribute`, `tag`,
    /// `catalogue`, `author`, `value` or `keyword`.
    pub fn detail(&self) -> &'static str {
        match self.kind {
            "status" | "time" | "length" => "value",
            kind => kind,
        }
    }
}

/// The suggestions for the word at `cursor` (a char index), and the chars `from..to` they
/// replace.
#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    pub from: usize,
    pub to: usize,
    pub suggestions: Vec<Suggestion>,
}

const SHOWN: usize = 8;
const KEYWORDS: [&str; 3] = ["AND", "OR", "NOT"];

pub fn complete(text: &str, cursor: usize, vocabulary: &Vocabulary) -> Option<Completion> {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let from = word_start(&chars, cursor);
    let word: String = chars[from..cursor].iter().collect();
    if KEYWORDS.contains(&word.as_str()) {
        return None;
    }
    let suggestions = match term_parts(&word) {
        Some((field, value)) => values(field, value, vocabulary),
        None if word.contains(':') => Vec::new(),
        None => keywords(&chars[..from], &word)
            .chain(names(&word, vocabulary))
            .collect(),
    };
    (!suggestions.is_empty()).then(|| Completion {
        from,
        to: cursor,
        suggestions: suggestions.into_iter().take(SHOWN).collect(),
    })
}

/// The keywords that fit the word `typed` after the text `before` it: `OR` and `AND` after
/// a term or a `)`, `NOT` there too and where a term starts (at the start, after `(` or a
/// keyword).
fn keywords(before: &[char], typed: &str) -> impl Iterator<Item = Suggestion> {
    let before: String = before.iter().collect();
    let before = before.trim_end();
    let last = before.rsplit(' ').next().unwrap_or_default();
    let term_starts = before.is_empty() || before.ends_with('(') || KEYWORDS.contains(&last);
    let fitting: &[&str] = if term_starts {
        &["NOT"]
    } else {
        &["OR", "AND", "NOT"]
    };
    let typed = typed.to_uppercase();
    fitting
        .iter()
        .filter(move |keyword| keyword.starts_with(&typed))
        .map(|keyword| suggestion(format!("{keyword} "), "keyword"))
}

/// The key of the attribute a term's name (`spacecraft`, `#spacecraft`, `"B jump"`) names,
/// or `None` for a field.
fn attribute_key(name: &str) -> Option<String> {
    let name = query::unquoted(name);
    match query::kind_of(&name) {
        Some(query::Kind::Attribute) => name.strip_prefix('#').map(str::to_owned),
        Some(_) => None,
        None => Some(name),
    }
}

/// Where the word at `cursor` starts: after the last space or bracket outside quotes.
fn word_start(chars: &[char], cursor: usize) -> usize {
    let mut start = 0;
    let mut quoted = false;
    for (index, &character) in chars[..cursor].iter().enumerate() {
        match character {
            '"' => quoted = !quoted,
            ' ' | '(' | ')' if !quoted => start = index + 1,
            _ => {}
        }
    }
    start
}

/// A field name, an attribute (bare unless a field has its name, or after a typed `#`), or
/// a known value, for a word with no `:` yet.
fn names(word: &str, vocabulary: &Vocabulary) -> Vec<Suggestion> {
    let typed = word.to_lowercase();
    if let Some(key) = typed.strip_prefix('#') {
        let key = key.trim_start_matches('"');
        return ranked(vocabulary.attributes.iter().map(|name| {
            let insert = format!("{}:", written_name(&format!("#{name}")));
            (name.as_str(), key, suggestion(insert, "attribute"))
        }));
    }
    let fields = crate::query::FIELDS
        .iter()
        .map(|field| field.name)
        .chain(["_exists_"])
        .map(|name| {
            (
                name,
                typed.as_str(),
                suggestion(format!("{name}:"), "field"),
            )
        });
    let known = (!typed.is_empty()).then(|| known_names(&typed, vocabulary));
    ranked(fields.chain(known.into_iter().flatten()))
}

/// The attributes, tags and catalogues a bare word may name, each with what it is matched
/// against and the typed word to match.
fn known_names<'a>(
    typed: &'a str,
    vocabulary: &'a Vocabulary,
) -> impl Iterator<Item = (&'a str, &'a str, Suggestion)> {
    let attributes = vocabulary.attributes.iter().map(move |name| {
        let insert = format!("{}:", attribute_name(name));
        (
            name.as_str(),
            typed.trim_start_matches('"'),
            suggestion(insert, "attribute"),
        )
    });
    let value = move |field: &str, kind, value: &'a String| {
        let insert = format!("{field}:{} ", quote(value));
        (value.as_str(), typed, suggestion(insert, kind))
    };
    let tags = vocabulary
        .tags
        .iter()
        .map(move |tag| value("tag", "tag", tag));
    let catalogues = vocabulary
        .catalogues
        .iter()
        .map(move |name| value("catalogue", "catalogue", name));
    attributes.chain(tags).chain(catalogues)
}

/// The candidates `(name, typed, suggestion)` whose name matches, those it begins first;
/// then by what they are: keywords, fields, attributes, values; else in the order given.
fn ranked<'a>(candidates: impl Iterator<Item = (&'a str, &'a str, Suggestion)>) -> Vec<Suggestion> {
    let mut found: Vec<_> = candidates
        .filter_map(|(name, typed, suggestion)| {
            let rank = (closeness(name, typed)?, kind_rank(suggestion.kind));
            Some((rank, suggestion))
        })
        .collect();
    found.sort_by_key(|(rank, _)| *rank);
    found
        .into_iter()
        .map(|(_, suggestion)| suggestion)
        .collect()
}

fn kind_rank(kind: &str) -> u8 {
    match kind {
        "keyword" => 0,
        "field" => 1,
        "attribute" => 2,
        _ => 3,
    }
}

/// The values a field takes, for a word `field:value` being typed.
fn values(field: &str, typed: &str, vocabulary: &Vocabulary) -> Vec<Suggestion> {
    let (op, typed) = split_operator(typed);
    let typed = typed.trim_start_matches('"').to_lowercase();
    let statuses: Vec<String> = STATUSES_SHOWN
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let fields: Vec<String> = crate::query::FIELDS
        .iter()
        .map(|field| field.name.to_owned())
        .chain(vocabulary.attributes.iter().map(|key| attribute_name(key)))
        .collect();
    let (pool, kind): (&[String], &'static str) = match field {
        "tag" | "tags" => (&vocabulary.tags, "tag"),
        "author" => (&vocabulary.authors, "author"),
        "catalogue" => (&vocabulary.catalogues, "catalogue"),
        "status" => (&statuses, "status"),
        "_exists_" => (&fields, "field"),
        "start" | "stop" | "created" => return times(field, op, &typed),
        "duration" => return durations(op, &typed),
        _ => match attribute_key(field).and_then(|key| vocabulary.attribute_values.get(&key)) {
            Some(values) => (values, "value"),
            None => return Vec::new(),
        },
    };
    let prefix_only = kind == "status" || kind == "value";
    matching(pool, &typed)
        .into_iter()
        .filter(|value| !prefix_only || value.to_lowercase().starts_with(&typed))
        .map(|value| match field {
            "_exists_" => suggestion(format!("{field}:{value} "), existing_kind(value)),
            _ => suggestion(format!("{field}:{op}{} ", quote(value)), kind),
        })
        .collect()
}

/// What a name after `_exists_:` is: a built-in field or an attribute.
fn existing_kind(name: &str) -> &'static str {
    match crate::query::FIELDS.iter().any(|field| field.name == name) {
        true => "field",
        false => "attribute",
    }
}

const STATUSES_SHOWN: [&str; 5] = ["preliminary", "confirmed", "reviewed", "final", "rejected"];

fn times(field: &str, op: &str, typed: &str) -> Vec<Suggestion> {
    let op = if op.is_empty() && field == "created" {
        ""
    } else {
        op
    };
    ["now-1d", "now-7d", "now-30d", "now-1y"]
        .into_iter()
        .filter(|time| time.starts_with(typed))
        .map(|time| suggestion(format!("{field}:{op}{time} "), "time"))
        .collect()
}

fn durations(op: &str, typed: &str) -> Vec<Suggestion> {
    let op = if op.is_empty() { ">" } else { op };
    ["1min", "10min", "1h", "1d"]
        .into_iter()
        .filter(|length| length.starts_with(typed))
        .map(|length| suggestion(format!("duration:{op}{length} "), "length"))
        .collect()
}

fn split_operator(typed: &str) -> (&str, &str) {
    [">=", "<=", ">", "<", "="]
        .into_iter()
        .find_map(|op| typed.strip_prefix(op).map(|rest| (op, rest)))
        .unwrap_or(("", typed))
}

/// A word contained in a name, not at its start, is offered from this many characters on:
/// shorter, it is in too many names to help.
const CONTAINED_FROM: usize = 3;

/// 0 when `name` begins with `typed`, 1 when it only contains it, without case.
fn closeness(name: &str, typed: &str) -> Option<u8> {
    let name = name.to_lowercase();
    if name.starts_with(typed) {
        Some(0)
    } else if typed.chars().count() >= CONTAINED_FROM && name.contains(typed) {
        Some(1)
    } else {
        None
    }
}

/// The values that begin with `typed`, then those that only contain it, without case.
fn matching<'a>(pool: &'a [String], typed: &str) -> Vec<&'a String> {
    let mut found: Vec<_> = pool
        .iter()
        .filter_map(|value| Some((closeness(value, typed)?, value)))
        .collect();
    found.sort_by_key(|(closeness, _)| *closeness);
    found.into_iter().map(|(_, value)| value).collect()
}

fn suggestion(insert: String, kind: &'static str) -> Suggestion {
    Suggestion { insert, kind }
}

/// The text with a suggestion taken, and where the cursor goes after it.
pub fn accept(text: &str, completion: &Completion, chosen: &Suggestion) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    let before: String = chars[..completion.from].iter().collect();
    let after: String = chars[completion.to..].iter().collect();
    let insert = match after.starts_with(' ') {
        true => chosen.insert.trim_end(),
        false => chosen.insert.as_str(),
    };
    let spaced = usize::from(after.starts_with(' '));
    let cursor = completion.from + insert.chars().count() + spaced;
    (format!("{before}{insert}{after}"), cursor)
}

#[cfg(test)]
mod completion_tests {
    use super::*;

    fn vocabulary() -> Vocabulary {
        Vocabulary {
            tags: vec!["shock".into(), "sheath".into(), "foreshock".into()],
            authors: vec!["alice@lab.fr".into(), "bob@lab.fr".into()],
            catalogues: vec!["Bow shocks".into(), "Solitons".into()],
            attributes: vec!["density".into(), "speed".into()],
            attribute_values: BTreeMap::new(),
        }
    }

    fn inserts(text: &str) -> Vec<String> {
        complete(text, text.chars().count(), &vocabulary())
            .map(|found| found.suggestions.into_iter().map(|s| s.insert).collect())
            .unwrap_or_default()
    }

    fn alexis() -> Vocabulary {
        Vocabulary {
            tags: vec!["origin_test".into()],
            catalogues: vec!["orbits".into()],
            attributes: ["origin", "Orbit No.", "normal_x", "normal_y", "suborbital"]
                .map(Into::into)
                .into(),
            ..Vocabulary::default()
        }
    }

    fn shown(text: &str, vocabulary: &Vocabulary) -> Vec<(String, &'static str)> {
        complete(text, text.chars().count(), vocabulary)
            .map(|found| {
                let detail = |s: &Suggestion| s.detail();
                found
                    .suggestions
                    .iter()
                    .map(|s| (s.insert.clone(), detail(s)))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn beginnings_come_first_then_fields_attributes_and_values() {
        let pair = |insert: &str, detail| (insert.to_owned(), detail);
        assert_eq!(
            shown("tag:a AND Or", &alexis()),
            [
                pair("origin:", "field"),
                pair("#origin:", "attribute"),
                pair("\"Orbit No.\":", "attribute"),
                pair("tag:origin_test ", "tag"),
                pair("catalogue:orbits ", "catalogue"),
            ],
            "two letters: only the names they begin"
        );
        assert_eq!(
            shown("orb", &alexis()),
            [
                pair("\"Orbit No.\":", "attribute"),
                pair("catalogue:orbits ", "catalogue"),
                pair("suborbital:", "attribute"),
            ],
            "from three letters, a name containing them, after every name they begin"
        );
        assert_eq!(inserts("tag:sh"), ["tag:shock ", "tag:sheath "]);
        assert_eq!(inserts("tag:sho"), ["tag:shock ", "tag:foreshock "]);
    }

    #[test]
    fn each_suggestion_says_what_it_is() {
        let details = |text: &str| -> Vec<&'static str> {
            shown(text, &vocabulary())
                .into_iter()
                .map(|(_, d)| d)
                .collect()
        };
        assert_eq!(details("tag:a ")[..3], ["keyword"; 3]);
        assert_eq!(details("author:"), ["author"; 2]);
        assert_eq!(details("status:f"), ["value"]);
        assert_eq!(details("created:now-1"), ["value"; 2]);
        assert_eq!(details("duration:1"), ["value"; 4]);
        assert_eq!(details("_exists_:sha"), ["field"]);
        assert_eq!(details("_exists_:dens"), ["attribute"]);
    }

    #[test]
    fn an_empty_box_lists_not_then_the_fields() {
        let all = inserts("");
        assert_eq!(all[..2], ["NOT ", "start:"]);
        assert_eq!(all.len(), SHOWN);
    }

    #[test]
    fn after_a_term_or_a_bracket_the_keywords_come_first() {
        let found = complete("tag:a ", 6, &vocabulary()).unwrap();
        assert_eq!(found.suggestions[0], suggestion("OR ".into(), "keyword"));
        assert_eq!(found.suggestions[1], suggestion("AND ".into(), "keyword"));
        assert_eq!(found.suggestions[2], suggestion("NOT ".into(), "keyword"));
        assert_eq!(found.suggestions[3].insert, "start:");
        assert_eq!(inserts("tag:a a")[..2], ["AND ", "author:"]);
        assert_eq!(inserts("(tag:a OR tag:b) ")[..3], ["OR ", "AND ", "NOT "]);
        assert_eq!(inserts("shock ")[..3], ["OR ", "AND ", "NOT "]);
        assert_eq!(inserts("tag:a o")[..2], ["OR ", "origin:"]);
        assert_eq!(inserts("tag:a n")[0], "NOT ");
        let text = "tag:a o";
        let found = complete(text, 7, &vocabulary()).unwrap();
        assert_eq!(
            accept(text, &found, &found.suggestions[0]),
            ("tag:a OR ".to_owned(), 9)
        );
    }

    #[test]
    fn an_attribute_value_completes_after_its_name() {
        let values = [
            ("spacecraft", vec!["MMS1", "MMS2", "Cluster 3"]),
            ("B jump", vec!["big"]),
        ];
        let with = Vocabulary {
            attributes: vec!["spacecraft".into(), "B jump".into()],
            attribute_values: values
                .into_iter()
                .map(|(key, values)| (key.into(), values.into_iter().map(Into::into).collect()))
                .collect(),
            ..vocabulary()
        };
        let inserts = |text: &str| -> Vec<String> {
            complete(text, text.chars().count(), &with)
                .map(|found| found.suggestions.into_iter().map(|s| s.insert).collect())
                .unwrap_or_default()
        };
        assert_eq!(
            inserts("spacecraft:"),
            [
                "spacecraft:MMS1 ",
                "spacecraft:MMS2 ",
                "spacecraft:\"Cluster 3\" "
            ]
        );
        assert_eq!(
            inserts("#spacecraft:mms"),
            ["#spacecraft:MMS1 ", "#spacecraft:MMS2 "]
        );
        assert_eq!(inserts("spacecraft:=cl"), ["spacecraft:=\"Cluster 3\" "]);
        assert_eq!(inserts("\"B jump\":b"), ["\"B jump\":big "]);
        assert_eq!(inserts("#\"B jump\":"), ["#\"B jump\":big "]);
        assert!(
            inserts("spacecraft:3").is_empty(),
            "values are matched by their start"
        );
        assert!(inserts("density:").is_empty(), "no values known");
        let found = complete("spacecraft:M", 12, &with).unwrap();
        assert_eq!(found.suggestions[0].kind, "value");
        for insert in inserts("spacecraft:")
            .into_iter()
            .chain(inserts("\"B jump\":"))
        {
            assert!(query::parse(&insert, &[]).is_ok(), "{insert} does not read");
        }
    }

    #[test]
    fn a_name_that_needs_quotes_is_suggested_quoted() {
        let spaced = Vocabulary {
            attributes: vec!["B jump".into()],
            ..vocabulary()
        };
        let suggested = |text: &str| {
            let found = complete(text, text.chars().count(), &spaced).unwrap();
            let first = found.suggestions[0].clone();
            (first.insert.clone(), accept(text, &found, &first).0)
        };
        let quoted = "#\"B jump\":".to_owned();
        assert_eq!(suggested("#B"), (quoted.clone(), quoted));
        let bare = "\"B jump\":".to_owned();
        assert_eq!(suggested("B"), (bare.clone(), bare.clone()));
        assert_eq!(suggested("\"B j"), (bare.clone(), bare));
        assert!(query::parse("\"B jump\":>1", &[]).is_ok());
    }

    #[test]
    fn at_the_start_or_after_a_keyword_only_not_is_offered() {
        for text in [
            "",
            "(",
            "tag:a OR ",
            "tag:a NOT ",
            "tag:a AND ",
            "(tag:a OR (",
        ] {
            let all = inserts(text);
            assert_eq!(all[0], "NOT ", "{text:?}");
            assert!(!all.contains(&"OR ".to_owned()), "{text:?}");
            assert!(!all.contains(&"AND ".to_owned()), "{text:?}");
        }
        assert!(inserts("st").iter().all(|insert| insert != "NOT "));
        assert!(
            inserts("tag:a NOT").is_empty(),
            "a typed keyword is complete"
        );
    }

    #[test]
    fn a_word_offers_fields_then_known_tags_and_catalogues() {
        assert_eq!(inserts("sh"), ["shape:", "tag:shock ", "tag:sheath "]);
        assert_eq!(
            inserts("sho"),
            ["tag:shock ", "tag:foreshock ", "catalogue:\"Bow shocks\" "]
        );
        assert_eq!(inserts("conf"), ["confidence:"]);
    }

    #[test]
    fn after_a_field_its_values_follow() {
        assert_eq!(inserts("tag:sho"), ["tag:shock ", "tag:foreshock "]);
        assert_eq!(
            inserts("status:r"),
            ["status:reviewed ", "status:rejected "]
        );
        assert_eq!(inserts("catalogue:bow"), ["catalogue:\"Bow shocks\" "]);
        assert_eq!(inserts("author:bo"), ["author:bob@lab.fr "]);
        assert_eq!(inserts("created:now-7"), ["created:now-7d "]);
        assert_eq!(
            inserts("duration:"),
            [
                "duration:>1min ",
                "duration:>10min ",
                "duration:>1h ",
                "duration:>1d "
            ]
        );
        assert_eq!(inserts("_exists_:sh"), ["_exists_:shape "]);
        assert_eq!(inserts("#sp"), ["#speed:"]);
    }

    #[test]
    fn only_the_word_at_the_cursor_counts() {
        assert_eq!(inserts("confidence:>=0.8 tag:fore"), ["tag:foreshock "]);
        assert_eq!(
            inserts("(tag:shock OR sta"),
            ["start:", "status:", "start_err:"]
        );
        assert!(inserts("tag:shock NOT").is_empty());
        let found = complete("catalogue:\"Bow sh", 17, &vocabulary()).unwrap();
        assert_eq!(found.from, 0);
    }

    #[test]
    fn a_bare_word_suggests_the_attributes_it_begins_bare_unless_a_field_has_the_name() {
        assert_eq!(inserts("dens"), ["density:"]);
        assert!(inserts("s").contains(&"speed:".to_owned()));
        assert_eq!(inserts("#sp"), ["#speed:"]);
        let found = complete("tag:shock Spe", 13, &vocabulary()).unwrap();
        assert_eq!(found.suggestions[0].kind, "attribute");
        assert_eq!(
            accept("tag:shock Spe", &found, &found.suggestions[0]),
            ("tag:shock speed:".to_owned(), 16)
        );
        let clashing = Vocabulary {
            attributes: vec!["status".into()],
            ..vocabulary()
        };
        let found = complete("statu", 5, &clashing).unwrap();
        let inserts: Vec<&str> = found
            .suggestions
            .iter()
            .map(|s| s.insert.as_str())
            .collect();
        assert_eq!(inserts, ["status:", "#status:"]);
        assert!(
            query::parse("density:>1", &[]).is_ok(),
            "a bare attribute reads"
        );
    }

    #[test]
    fn accepting_replaces_the_word_and_moves_the_cursor_after_it() {
        let text = "confidence:>=0.8 tag:fo status:final";
        let found = complete(text, 23, &vocabulary()).unwrap();
        let (text, cursor) = accept(text, &found, &found.suggestions[0]);
        assert_eq!(text, "confidence:>=0.8 tag:foreshock status:final");
        assert_eq!(cursor, 31);
        let found = complete("tag:fo", 6, &vocabulary()).unwrap();
        assert_eq!(
            accept("tag:fo", &found, &found.suggestions[0]),
            ("tag:foreshock ".to_owned(), 14)
        );
    }

    #[test]
    fn every_suggestion_reads() {
        for typed in [
            "",
            "sh",
            "tag:",
            "status:",
            "catalogue:",
            "author:",
            "created:",
            "duration:",
            "_exists_:",
            "#",
        ] {
            for insert in inserts(typed) {
                if insert.ends_with(':') || KEYWORDS.contains(&insert.trim_end()) {
                    continue;
                }
                let names = vec![
                    ("c1".to_owned(), "Bow shocks".to_owned()),
                    ("c2".to_owned(), "Solitons".to_owned()),
                ];
                assert!(
                    crate::query::parse(&insert, &names).is_ok(),
                    "{insert} does not read"
                );
            }
        }
    }
}

/// One part of the search text as the box colours it. Joined back, the parts give the text
/// unchanged, character for character, so the coloured copy lines up under the input.
#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    Space(String),
    Keyword(String),
    Bracket(String),
    Word(String),
    /// `field:value`, split as `field`, `:` plus any comparison, and the value.
    Term {
        field: String,
        op: String,
        value: String,
    },
}

impl Piece {
    pub fn text(&self) -> String {
        match self {
            Piece::Space(text)
            | Piece::Keyword(text)
            | Piece::Bracket(text)
            | Piece::Word(text) => text.clone(),
            Piece::Term { field, op, value } => format!("{field}{op}{value}"),
        }
    }
}

pub fn pieces(text: &str) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut rest = text;
    while let Some(first) = rest.chars().next() {
        let length = match first {
            ' ' => rest.len() - rest.trim_start_matches(' ').len(),
            '(' | ')' => 1,
            _ => word_length(rest),
        };
        let (piece, after) = rest.split_at(length);
        pieces.push(classify(piece));
        rest = after;
    }
    pieces
}

/// A word runs to the next space or bracket outside quotes and `[… TO …]` ranges.
fn word_length(text: &str) -> usize {
    let (mut quoted, mut ranged) = (false, false);
    for (index, character) in text.char_indices() {
        match character {
            '"' => quoted = !quoted,
            '[' if !quoted => ranged = true,
            ']' if !quoted => ranged = false,
            ' ' | '(' | ')' if !quoted && !ranged => return index,
            _ => {}
        }
    }
    text.len()
}

fn classify(piece: &str) -> Piece {
    let owned = piece.to_owned();
    match piece {
        _ if piece.starts_with(' ') => Piece::Space(owned),
        "(" | ")" => Piece::Bracket(owned),
        _ if KEYWORDS.contains(&piece) => Piece::Keyword(owned),
        _ => match term_parts(piece) {
            Some((field, value)) => {
                let (op, value) = split_operator(value);
                Piece::Term {
                    field: field.to_owned(),
                    op: format!(":{op}"),
                    value: value.to_owned(),
                }
            }
            None => Piece::Word(owned),
        },
    }
}

/// A term's name and what follows its `:`; a quoted name (`"B jump"`, `#"B jump"`) runs to
/// its closing quote.
fn term_parts(word: &str) -> Option<(&str, &str)> {
    let body = word.strip_prefix('#').unwrap_or(word);
    if !body.starts_with('"') {
        return word
            .split_once(':')
            .filter(|(field, _)| is_field_name(field));
    }
    let end = word.len() - body.len() + closing_quote(body)? + 1;
    let rest = word[end..].strip_prefix(':')?;
    Some((&word[..end], rest))
}

/// Where the quote opening `quoted` closes, `\"` being a quote inside.
fn closing_quote(quoted: &str) -> Option<usize> {
    let mut escaped = false;
    for (index, character) in quoted.char_indices().skip(1) {
        match character {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => return Some(index),
            _ => {}
        }
    }
    None
}

/// What the search language reads as a field name: a letter, `_` or `#` first.
fn is_field_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_' || first == '#')
}

#[cfg(test)]
mod piece_tests {
    use super::*;

    fn joined(text: &str) -> String {
        pieces(text).iter().map(Piece::text).collect()
    }

    #[test]
    fn a_quoted_name_is_the_field_of_a_term() {
        let term = |field: &str, op: &str, value: &str| Piece::Term {
            field: field.to_owned(),
            op: op.to_owned(),
            value: value.to_owned(),
        };
        assert_eq!(pieces("\"B jump\":>1"), [term("\"B jump\"", ":>", "1")]);
        assert_eq!(pieces("#\"a:b\":x"), [term("#\"a:b\"", ":", "x")]);
        assert_eq!(pieces("\"a:b\""), [Piece::Word("\"a:b\"".to_owned())]);
    }

    #[test]
    fn the_pieces_give_back_the_text_exactly() {
        for text in [
            "",
            "shock",
            "tag:shock  confidence:>=0.8",
            "(tag:shock OR status:final) NOT author:\"a b\"",
            "start:[2015 TO 2016-06] created:now-7d",
            "catalogue:\"Bow (inner) shocks\" 14:02 #density:>10 ",
            "tag:",
            "\"unclosed quote",
        ] {
            assert_eq!(joined(text), text);
        }
    }

    #[test]
    fn terms_keywords_brackets_and_words_are_told_apart() {
        assert_eq!(
            pieces("(tag:shock OR confidence:>=0.8) sheath"),
            [
                Piece::Bracket("(".into()),
                Piece::Term {
                    field: "tag".into(),
                    op: ":".into(),
                    value: "shock".into()
                },
                Piece::Space(" ".into()),
                Piece::Keyword("OR".into()),
                Piece::Space(" ".into()),
                Piece::Term {
                    field: "confidence".into(),
                    op: ":>=".into(),
                    value: "0.8".into()
                },
                Piece::Bracket(")".into()),
                Piece::Space(" ".into()),
                Piece::Word("sheath".into()),
            ]
        );
        assert_eq!(
            pieces("start:[2015 TO 2016]"),
            [Piece::Term {
                field: "start".into(),
                op: ":".into(),
                value: "[2015 TO 2016]".into()
            }]
        );
        assert_eq!(pieces("14:02"), [Piece::Word("14:02".into())]);
    }
}

/// What the plain-words filter reads of an event: what its row in the table shows.
#[derive(Debug, Clone, Copy)]
pub struct Shown<'r> {
    /// Nanoseconds.
    pub start: i64,
    pub tags: &'r [String],
    pub status: Status,
    /// The event's author, else the name of what made it.
    pub author: Option<&'r str>,
    pub attributes: &'r Map<String, Value>,
}

/// The table's plain-words filter: every word must be part of a tag, the status, the author
/// or an attribute value, or begin the start time; the whole text may also begin the start
/// time ("2019-11-03 14:02"). Case does not matter.
#[derive(Debug, Clone)]
pub struct Words {
    text: String,
    words: Vec<String>,
}

impl Words {
    pub fn new(text: &str) -> Self {
        let text = text.trim().to_lowercase();
        let words = text.split_whitespace().map(str::to_owned).collect();
        Self { text, words }
    }

    pub fn matches(&self, shown: &Shown<'_>) -> bool {
        let start = start_text(shown.start);
        start.starts_with(&self.text) || self.words.iter().all(|word| word_in(shown, &start, word))
    }
}

fn word_in(shown: &Shown<'_>, start: &str, word: &str) -> bool {
    let contains = |text: &str| text.to_lowercase().contains(word);
    start.starts_with(word)
        || shown.tags.iter().any(|tag| contains(tag))
        || contains(shown.status.name())
        || shown.author.is_some_and(contains)
        || shown.attributes.values().any(|value| match value {
            Value::String(text) => contains(text),
            other => contains(&other.to_string()),
        })
}

const START: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");

/// A start time as the table shows it, UTC to the second: "2019-11-03 14:02:00".
pub fn start_text(nanos: i64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(nanos))
        .ok()
        .and_then(|time| time.format(START).ok())
        .unwrap_or_else(|| nanos.to_string())
}

#[cfg(test)]
mod words_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_word_must_match_a_shown_value_or_begin_the_start() {
        let attributes: Map<String, serde_json::Value> =
            serde_json::from_value(json!({"mode": "Burst", "n": 12})).unwrap();
        let tags = ["shock".to_owned()];
        let shown = Shown {
            start: 1_572_789_720_000_000_000, // 2019-11-03 14:02:00
            tags: &tags,
            status: Status::Reviewed,
            author: Some("Lavraud"),
            attributes: &attributes,
        };
        let keeps = |text: &str| Words::new(text).matches(&shown);
        assert!(keeps("burst"));
        assert!(keeps("SHOCK reviewed"));
        assert!(keeps("lavraud 12"));
        assert!(keeps("2019-11-03 14:02"));
        assert!(keeps(""));
        assert!(!keeps("14:02"));
        assert!(!keeps("shock final"));
    }
}
