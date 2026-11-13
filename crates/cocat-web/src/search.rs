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

/// What the project holds that a search can name, for the completions.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Vocabulary {
    pub tags: Vec<String>,
    pub authors: Vec<String>,
    pub catalogues: Vec<String>,
    pub attributes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    /// What replaces the word being typed.
    pub insert: String,
    /// What kind of thing it is: "field", "tag", "catalogue"…
    pub kind: &'static str,
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
    let suggestions = match word.split_once(':') {
        Some((field, value)) => values(field, value, vocabulary),
        None => names(&word, vocabulary),
    };
    (!suggestions.is_empty()).then(|| Completion {
        from,
        to: cursor,
        suggestions: suggestions.into_iter().take(SHOWN).collect(),
    })
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

/// A field name, an attribute, or a known value, for a word with no `:` yet.
fn names(word: &str, vocabulary: &Vocabulary) -> Vec<Suggestion> {
    let typed = word.to_lowercase();
    if let Some(key) = typed.strip_prefix('#') {
        return matching(&vocabulary.attributes, key)
            .map(|name| suggestion(format!("#{name}:"), "attribute"))
            .collect();
    }
    let fields = cocat_core::query::FIELDS
        .iter()
        .map(|field| field.name)
        .chain(["_exists_"])
        .filter(|name| name.starts_with(&typed))
        .map(|name| suggestion(format!("{name}:"), "field"));
    let known = (!typed.is_empty()).then(|| {
        let tags = matching(&vocabulary.tags, &typed).map(|tag| ("tag", tag, "tag"));
        let catalogues =
            matching(&vocabulary.catalogues, &typed).map(|name| ("catalogue", name, "catalogue"));
        tags.chain(catalogues)
            .map(|(field, value, kind)| suggestion(format!("{field}:{} ", quote(value)), kind))
            .collect::<Vec<_>>()
    });
    fields.chain(known.into_iter().flatten()).collect()
}

/// The values a field takes, for a word `field:value` being typed.
fn values(field: &str, typed: &str, vocabulary: &Vocabulary) -> Vec<Suggestion> {
    let (op, typed) = split_operator(typed);
    let typed = typed.trim_start_matches('"').to_lowercase();
    let statuses: Vec<String> = STATUSES_SHOWN
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let fields: Vec<String> = cocat_core::query::FIELDS
        .iter()
        .map(|field| field.name.to_owned())
        .chain(vocabulary.attributes.iter().map(|key| format!("#{key}")))
        .collect();
    let (pool, kind): (&[String], &'static str) = match field {
        "tag" | "tags" => (&vocabulary.tags, "tag"),
        "author" => (&vocabulary.authors, "author"),
        "catalogue" => (&vocabulary.catalogues, "catalogue"),
        "status" => (&statuses, "status"),
        "_exists_" => (&fields, "field"),
        "start" | "stop" | "created" => return times(field, op, &typed),
        "duration" => return durations(op, &typed),
        _ => return Vec::new(),
    };
    let prefix_only = field == "status";
    matching(pool, &typed)
        .filter(|value| !prefix_only || value.starts_with(&typed))
        .map(|value| {
            let value = if field == "_exists_" {
                value.to_owned()
            } else {
                quote(value)
            };
            suggestion(format!("{field}:{op}{value} "), kind)
        })
        .collect()
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

/// The values that begin with `typed`, then those that only contain it, without case.
fn matching<'a>(pool: &'a [String], typed: &'a str) -> impl Iterator<Item = &'a String> {
    let lower = |value: &String| value.to_lowercase();
    let starts = pool
        .iter()
        .filter(move |value| lower(value).starts_with(typed));
    let contains = pool.iter().filter(move |value| {
        let value = lower(value);
        !value.starts_with(typed) && value.contains(typed)
    });
    starts.chain(contains)
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
        }
    }

    fn inserts(text: &str) -> Vec<String> {
        complete(text, text.chars().count(), &vocabulary())
            .map(|found| found.suggestions.into_iter().map(|s| s.insert).collect())
            .unwrap_or_default()
    }

    #[test]
    fn an_empty_box_lists_the_fields() {
        let all = inserts("");
        assert_eq!(all[0], "start:");
        assert_eq!(all.len(), SHOWN);
    }

    #[test]
    fn a_word_offers_fields_then_known_tags_and_catalogues() {
        assert_eq!(
            inserts("sh"),
            [
                "shape:",
                "tag:shock ",
                "tag:sheath ",
                "tag:foreshock ",
                "catalogue:\"Bow shocks\" "
            ]
        );
        assert_eq!(inserts("conf"), ["confidence:"]);
    }

    #[test]
    fn after_a_field_its_values_follow() {
        assert_eq!(
            inserts("tag:sh"),
            ["tag:shock ", "tag:sheath ", "tag:foreshock "]
        );
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
                if insert.ends_with(':') {
                    continue;
                }
                let names = vec![
                    ("c1".to_owned(), "Bow shocks".to_owned()),
                    ("c2".to_owned(), "Solitons".to_owned()),
                ];
                assert!(
                    cocat_core::query::parse(&insert, &names).is_ok(),
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
        _ => match piece
            .split_once(':')
            .filter(|(field, _)| is_field_name(field))
        {
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

/// What the box on a catalogue's page does: filter what the catalogue holds, or edit the
/// search that brings events into it, from the whole project or from every public catalogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Filter,
    Search,
    Public,
}

impl Side {
    /// Whether the box edits the catalogue's own search.
    pub fn searches(self) -> bool {
        self != Side::Filter
    }
}

/// What a catalogue page lists before the box filters it, from the project; the public
/// catalogues' part is [`public_query`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listing {
    /// What the catalogue holds now.
    Members,
    /// What it would hold with the box's search: its picked events and what the search finds.
    Preview,
    /// Only its picked events: the box searches the public catalogues.
    Picked,
}

/// The catalogue's own search, if it has one: its text, and whether it searches the public
/// catalogues.
pub type Saved<'s> = Option<(&'s str, bool)>;

pub fn listing(side: Side, saved: Saved<'_>, text: &str) -> Listing {
    let text = text.trim();
    match side {
        Side::Filter => Listing::Members,
        Side::Search if saved == Some((text, false)) || (saved.is_none() && text.is_empty()) => {
            Listing::Members
        }
        Side::Search => Listing::Preview,
        Side::Public => Listing::Picked,
    }
}

/// The search the server runs through the public catalogues for the page, if any: the box's
/// on that side, else the catalogue's own when it searches them.
pub fn public_query(side: Side, saved: Saved<'_>, text: &str) -> Option<String> {
    let text = text.trim();
    match (side, saved) {
        (Side::Public, _) => (!text.is_empty()).then(|| text.to_owned()),
        (Side::Filter, Some((saved, true))) => Some(saved.to_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod listing_tests {
    use super::{Listing, Side, listing, public_query};

    #[test]
    fn a_filter_never_changes_what_the_catalogue_lists() {
        assert_eq!(
            listing(Side::Filter, Some(("tag:x", false)), "tag:y"),
            Listing::Members
        );
        assert_eq!(listing(Side::Filter, None, "tag:y"), Listing::Members);
    }

    #[test]
    fn its_search_as_saved_lists_what_it_holds() {
        assert_eq!(
            listing(Side::Search, Some(("tag:x", false)), " tag:x "),
            Listing::Members
        );
        assert_eq!(listing(Side::Search, None, " "), Listing::Members);
    }

    #[test]
    fn an_edited_search_previews_what_it_would_hold() {
        assert_eq!(
            listing(Side::Search, Some(("tag:x", false)), "tag:y"),
            Listing::Preview
        );
        assert_eq!(listing(Side::Search, None, "tag:y"), Listing::Preview);
        assert_eq!(
            listing(Side::Search, Some(("tag:x", false)), ""),
            Listing::Preview
        );
        assert_eq!(
            listing(Side::Search, Some(("tag:x", true)), "tag:x"),
            Listing::Preview,
            "the same words through the project are another search"
        );
    }

    #[test]
    fn the_public_catalogues_are_searched_by_the_server_beside_the_picked_events() {
        assert_eq!(listing(Side::Public, None, "tag:x"), Listing::Picked);
        assert_eq!(
            public_query(Side::Public, None, " tag:x ").as_deref(),
            Some("tag:x")
        );
        assert_eq!(public_query(Side::Public, None, " "), None);
        assert_eq!(
            public_query(Side::Search, Some(("tag:x", true)), "tag:y"),
            None
        );
    }

    #[test]
    fn a_catalogue_searching_them_holds_what_the_server_finds_under_any_filter() {
        let saved = Some(("tag:x", true));
        assert_eq!(
            public_query(Side::Filter, saved, "wave").as_deref(),
            Some("tag:x")
        );
        assert_eq!(public_query(Side::Filter, Some(("tag:x", false)), ""), None);
    }
}
