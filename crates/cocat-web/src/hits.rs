//! What a search found in a piece of text, so the page can mark it: the parts of a cell, or of
//! a public catalogue's name, that made the row match.

use std::ops::Range;

use cocat_core::rules::{Op, Predicate};
use regex::{Regex, RegexBuilder};
use serde_json::Value;

use crate::search::Query;

/// The patterns a search looks for, by the column they apply to.
#[derive(Debug, Default)]
pub struct Hits {
    /// Plain words: they match any text of the row, and the start time they begin.
    anywhere: Vec<Regex>,
    /// A field's own term, `author:alice` or `#spacecraft:MMS1`: (field, pattern).
    fields: Vec<(String, Regex)>,
}

/// Two are the same when they look for the same patterns, so a page redraws its marks only
/// when the search changes them.
impl PartialEq for Hits {
    fn eq(&self, other: &Self) -> bool {
        patterns(self) == patterns(other)
    }
}

fn patterns(hits: &Hits) -> Vec<(&str, &str)> {
    let anywhere = hits.anywhere.iter().map(|regex| ("", regex.as_str()));
    let fields = hits
        .fields
        .iter()
        .map(|(field, regex)| (field.as_str(), regex.as_str()));
    anywhere.chain(fields).collect()
}

/// The columns a row's texts sit in, named as the search names their field.
pub const START: &str = "start";
pub const TAGS: &str = "tags";
pub const STATUS: &str = "status";
pub const AUTHOR: &str = "author";

impl Hits {
    /// What the filter box's search marks; nothing for a search that does not read.
    pub fn of(query: &Query, text: &str) -> Self {
        match query {
            Query::Words => Self {
                anywhere: text.split_whitespace().filter_map(contains).collect(),
                fields: Vec::new(),
            },
            Query::Rule(predicate) => {
                let mut hits = Self::default();
                hits.collect(predicate);
                hits
            }
            Query::Empty | Query::Broken(_) => Self::default(),
        }
    }

    /// The words of a catalogue search, each marked where a word of the text begins with it.
    pub fn words(text: &str) -> Self {
        let anywhere = text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .filter_map(|word| pattern(&format!(r"\b{}", regex::escape(word))))
            .collect();
        Self {
            anywhere,
            fields: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.anywhere.is_empty() && self.fields.is_empty()
    }

    /// `text`, shown in the column of `field` (an attribute as `#key`), cut into runs, each
    /// with whether the search found it.
    pub fn runs<'t>(&self, field: &str, text: &'t str) -> Vec<(&'t str, bool)> {
        let anywhere = self.anywhere.iter().flat_map(|regex| {
            regex
                .find_iter(text)
                .filter(|found| field != START || found.start() == 0)
                .map(|found| found.range())
        });
        let own = self
            .fields
            .iter()
            .filter(|(name, _)| name == field)
            .flat_map(|(_, regex)| regex.find_iter(text).map(|found| found.range()));
        let mut ranges: Vec<_> = anywhere
            .chain(own)
            .filter(|range| !range.is_empty())
            .collect();
        ranges.sort_by_key(|range| range.start);
        cut(text, &ranges)
    }

    /// Whether the search found anything in `text`, in the column of `field`.
    pub fn found(&self, field: &str, text: &str) -> bool {
        self.runs(field, text).iter().any(|(_, hit)| *hit)
    }

    /// Terms under a NOT are what the row lacks: nothing in it to mark.
    fn collect(&mut self, predicate: &Predicate) {
        match predicate {
            Predicate::And(all) | Predicate::Or(all) => all.iter().for_each(|p| self.collect(p)),
            Predicate::Text(word) => self.anywhere.extend(contains(word)),
            Predicate::Match(field, regex) => self.add(field, pattern(regex)),
            Predicate::Contains(field, Value::String(text))
            | Predicate::Cmp(field, Op::Eq, Value::String(text)) => {
                self.add(field, pattern(&format!("^{}$", regex::escape(text))));
            }
            _ => {}
        }
    }

    fn add(&mut self, field: &str, regex: Option<Regex>) {
        if let Some(regex) = regex {
            self.fields.push((field.to_owned(), regex));
        }
    }
}

fn contains(word: &str) -> Option<Regex> {
    pattern(&regex::escape(word))
}

fn pattern(text: &str) -> Option<Regex> {
    RegexBuilder::new(text).case_insensitive(true).build().ok()
}

/// `text` cut at `ranges` (sorted by start, possibly overlapping), merged where they touch.
fn cut<'t>(text: &'t str, ranges: &[Range<usize>]) -> Vec<(&'t str, bool)> {
    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range.clone()),
        }
    }
    let mut runs = Vec::new();
    let mut at = 0;
    for range in merged {
        if range.start > at {
            runs.push((&text[at..range.start], false));
        }
        runs.push((&text[range.clone()], true));
        at = range.end;
    }
    if at < text.len() || runs.is_empty() {
        runs.push((&text[at..], false));
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::read;

    fn hits(text: &str) -> Hits {
        Hits::of(&read(text, &[]), text)
    }

    fn marked(hits: &Hits, field: &str, text: &str) -> Vec<String> {
        hits.runs(field, text)
            .into_iter()
            .filter(|(_, hit)| *hit)
            .map(|(run, _)| run.to_owned())
            .collect()
    }

    #[test]
    fn plain_words_mark_every_text_ignoring_case() {
        let hits = hits("shock mms");
        assert_eq!(marked(&hits, TAGS, "Bow Shock"), ["Shock"]);
        assert_eq!(marked(&hits, "#spacecraft", "MMS1"), ["MMS"]);
        assert_eq!(marked(&hits, AUTHOR, "alice"), Vec::<String>::new());
    }

    #[test]
    fn the_runs_give_back_the_text_exactly() {
        let hits = hits("ab bc");
        let text = "xabcx abc";
        let runs = hits.runs(AUTHOR, text);
        assert_eq!(runs.iter().map(|(run, _)| *run).collect::<String>(), text);
        assert_eq!(
            runs,
            [("x", false), ("abc", true), ("x ", false), ("abc", true)]
        );
    }

    #[test]
    fn a_word_marks_the_start_time_only_where_it_begins() {
        let hits = hits("2015-03");
        assert_eq!(marked(&hits, START, "2015-03-01 12:00"), ["2015-03"]);
        let hits = Hits::of(&read("03", &[]), "03");
        assert_eq!(marked(&hits, START, "2015-03-01"), Vec::<String>::new());
    }

    #[test]
    fn a_field_term_marks_only_its_column() {
        let hits = hits("author:alice tag:shock status:reviewed #spacecraft:MMS1");
        assert_eq!(marked(&hits, AUTHOR, "Alice@lab.fr"), ["Alice"]);
        assert_eq!(marked(&hits, TAGS, "alice"), Vec::<String>::new());
        assert_eq!(marked(&hits, TAGS, "shock"), ["shock"]);
        assert_eq!(
            marked(&hits, TAGS, "shocks"),
            Vec::<String>::new(),
            "a tag is exact"
        );
        assert!(hits.found(STATUS, "reviewed"));
        assert!(hits.found("#spacecraft", "MMS1"));
    }

    #[test]
    fn what_a_search_excludes_is_not_marked() {
        let hits = hits("shock NOT tag:wave");
        assert!(hits.found(TAGS, "shock"));
        assert!(!hits.found(TAGS, "wave"));
    }

    #[test]
    fn nothing_is_marked_without_a_readable_search() {
        assert!(hits("").is_empty());
        assert!(hits("author:(").is_empty());
        assert!(hits("confidence:>0.5").is_empty());
    }

    #[test]
    fn catalogue_search_words_mark_where_a_word_begins() {
        let hits = Hits::words("recon solar");
        assert_eq!(
            marked(&hits, "name", "Reconnection exhausts, Solar Orbiter"),
            ["Recon", "Solar"]
        );
        assert_eq!(marked(&hits, "name", "unreconciled"), Vec::<String>::new());
    }
}
