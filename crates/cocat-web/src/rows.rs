//! The rows of the event table: a catalogue's members or the trash, and how the table
//! filters and sorts them. Filtering and sorting give positions into the rows, so the
//! rows themselves are read once per room change.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};

use cocat_core::model::{self, EventRecord, Member, Status};
use cocat_core::rules::{Predicate, RuleError};
use cocat_core::time::Timestamp;
use serde_json::{Map, Value};
use yrs::ReadTxn;

use crate::format::{time_text, value_text};

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// `None` for an interval a rule derived: it is not an event of the room.
    pub uuid: Option<String>,
    pub start: i64,
    pub stop: i64,
    pub tags: Vec<String>,
    pub confidence: Option<f64>,
    pub status: Status,
    /// The event's author, else the name of what made it.
    pub author: Option<String>,
    pub attributes: Map<String, Value>,
    /// The events a derived interval came from.
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SortKey {
    Start,
    Duration,
    Tags,
    Confidence,
    Status,
    Author,
    Attribute(String),
}

/// The members of a catalogue in time order; none for a catalogue the room does not hold.
pub fn rows<T: ReadTxn>(txn: &T, catalogue: &str) -> Result<Vec<Row>, RuleError> {
    let Some(members) = model::catalogue_members(txn, catalogue) else {
        return Ok(Vec::new());
    };
    let mut origins = OriginNames::new(txn);
    Ok(members?
        .into_iter()
        .map(|member| match member {
            Member::Event { record, .. } => event_row(record, &mut origins),
            Member::Derived {
                start,
                stop,
                sources,
            } => Row {
                uuid: None,
                start: nanos(start),
                stop: nanos(stop),
                tags: Vec::new(),
                confidence: None,
                status: Status::default(),
                author: None,
                attributes: Map::new(),
                sources,
            },
        })
        .collect())
}

/// One event as the table shows it, deleted or not.
pub fn event<T: ReadTxn>(txn: &T, uuid: &str) -> Option<Row> {
    let record = model::event(txn, uuid)?;
    Some(event_row(record, &mut OriginNames::new(txn)))
}

pub fn trash_rows<T: ReadTxn>(txn: &T) -> Vec<Row> {
    let mut origins = OriginNames::new(txn);
    model::trash(txn)
        .into_iter()
        .map(|record| event_row(record, &mut origins))
        .collect()
}

/// Every live event of the project, in time order: the "All events" page.
pub fn all_rows<T: ReadTxn>(txn: &T) -> Result<Vec<Row>, RuleError> {
    search_rows(txn, &Predicate::And(Vec::new()), None)
}

/// The positions of the rows a search keeps, tested as a smart catalogue would test them.
pub fn matching<T: ReadTxn>(
    txn: &T,
    rows: &[Row],
    predicate: &Predicate,
) -> Result<Vec<usize>, RuleError> {
    let search = model::Search::new(txn, predicate, Timestamp::now())?;
    Ok((0..rows.len())
        .filter(|&index| {
            let row = &rows[index];
            search.matches(txn, row.uuid.as_deref(), row.start, row.stop)
        })
        .collect())
}

/// The events a search finds, in all of the project or among one catalogue's members: what a
/// smart catalogue with that search would hold, before it is saved.
/// What catalogue `catalogue` would hold with `search` as its search: its picked live events
/// and every event the search finds, in time order.
pub fn preview_rows<T: ReadTxn>(
    txn: &T,
    catalogue: &str,
    search: Option<&Predicate>,
) -> Result<Vec<Row>, RuleError> {
    let now = Timestamp::now();
    let picked: HashSet<String> = model::listed_events(txn, catalogue).into_iter().collect();
    let found: HashSet<String> = match search {
        Some(search) => model::search(txn, search, now)?
            .into_iter()
            .map(|record| record.uuid)
            .collect(),
        None => HashSet::new(),
    };
    let mut origins = OriginNames::new(txn);
    Ok(model::search(txn, &Predicate::And(Vec::new()), now)?
        .into_iter()
        .filter(|record| picked.contains(&record.uuid) || found.contains(&record.uuid))
        .map(|record| event_row(record, &mut origins))
        .collect())
}

pub fn search_rows<T: ReadTxn>(
    txn: &T,
    predicate: &Predicate,
    source: Option<&str>,
) -> Result<Vec<Row>, RuleError> {
    let found = model::search(txn, predicate, Timestamp::now())?;
    let within: Option<Vec<String>> = match source {
        Some(catalogue) => Some(
            rows(txn, catalogue)?
                .into_iter()
                .filter_map(|row| row.uuid)
                .collect(),
        ),
        None => None,
    };
    let mut origins = OriginNames::new(txn);
    Ok(found
        .into_iter()
        .filter(|record| {
            within
                .as_ref()
                .is_none_or(|within| within.contains(&record.uuid))
        })
        .map(|record| event_row(record, &mut origins))
        .collect())
}

/// The positions of the rows every word of `query` matches, in order. A word matches a
/// tag, the status, the author or an attribute value it is part of, or the start time it
/// begins; the whole query may also begin the start time ("2019-11-03 14:02").
pub fn filter(rows: &[Row], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    let words: Vec<&str> = query.split_whitespace().collect();
    (0..rows.len())
        .filter(|&index| {
            let row = &rows[index];
            let start = time_text(row.start);
            start.starts_with(&query) || words.iter().all(|word| matches(row, &start, word))
        })
        .collect()
}

/// Sorts positions into `rows`; rows without the value sort last either way, and ties
/// keep their order.
pub fn sort(rows: &[Row], order: &mut [usize], key: &SortKey, descending: bool) {
    order.sort_by(|&left, &right| {
        let (left, right) = (&rows[left], &rows[right]);
        match (sort_value(left, key), sort_value(right, key)) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(left), Some(right)) if descending => right.cmp(&left),
            (Some(left), Some(right)) => left.cmp(&right),
        }
    });
}

/// Every attribute key the rows hold, sorted: the columns the table can show.
pub fn attribute_keys(rows: &[Row]) -> Vec<String> {
    let keys: BTreeSet<&String> = rows.iter().flat_map(|row| row.attributes.keys()).collect();
    keys.into_iter().cloned().collect()
}

fn matches(row: &Row, start: &str, word: &str) -> bool {
    let contains = |text: &str| text.to_lowercase().contains(word);
    start.starts_with(word)
        || row.tags.iter().any(|tag| contains(tag))
        || contains(row.status.name())
        || row.author.as_deref().is_some_and(contains)
        || row
            .attributes
            .values()
            .any(|value| contains(&value_text(value)))
}

#[derive(PartialEq, PartialOrd)]
enum SortValue {
    Number(f64),
    Text(String),
}

impl SortValue {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other).unwrap_or(Ordering::Equal)
    }
}

fn sort_value(row: &Row, key: &SortKey) -> Option<SortValue> {
    let position = |status| Status::ALL.iter().position(|known| *known == status);
    match key {
        SortKey::Start => Some(SortValue::Number(row.start as f64)),
        SortKey::Duration => Some(SortValue::Number((row.stop - row.start) as f64)),
        SortKey::Tags => (!row.tags.is_empty()).then(|| SortValue::Text(row.tags.join(" "))),
        SortKey::Confidence => row.confidence.map(SortValue::Number),
        SortKey::Status => position(row.status).map(|index| SortValue::Number(index as f64)),
        SortKey::Author => row.author.clone().map(SortValue::Text),
        SortKey::Attribute(key) => row.attributes.get(key).map(|value| match value {
            Value::Number(number) => SortValue::Number(number.as_f64().unwrap_or_default()),
            other => SortValue::Text(value_text(other)),
        }),
    }
}

fn event_row<T: ReadTxn>(mut record: EventRecord, origins: &mut OriginNames<'_, T>) -> Row {
    let author = record
        .author
        .take()
        .or_else(|| origins.name(record.origin.as_deref()?));
    Row {
        author,
        ..record_row(record)
    }
}

/// An event read from the server rather than the project: its origin's name is unknown.
pub fn record_row(record: EventRecord) -> Row {
    Row {
        uuid: Some(record.uuid),
        start: nanos(record.start),
        stop: nanos(record.stop),
        tags: record.tags,
        confidence: record.confidence,
        status: record.status,
        author: record.author,
        attributes: record.attributes,
        sources: Vec::new(),
    }
}

/// A room stores times as nanoseconds, so every time it gives back fits.
fn nanos(time: cocat_core::time::Timestamp) -> i64 {
    time.unix_nanos().unwrap_or_default()
}

/// The names of origins, looked up once each: thousands of events share a few.
struct OriginNames<'t, T> {
    txn: &'t T,
    names: HashMap<String, Option<String>>,
}

impl<'t, T: ReadTxn> OriginNames<'t, T> {
    fn new(txn: &'t T) -> Self {
        Self {
            txn,
            names: HashMap::new(),
        }
    }

    fn name(&mut self, id: &str) -> Option<String> {
        let txn = self.txn;
        self.names
            .entry(id.to_owned())
            .or_insert_with(|| model::origin(txn, id).map(|origin| origin.name))
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use cocat_core::model::Status;
    use serde_json::json;
    use yrs::Transact;

    use super::*;
    use crate::fixture::{DAY0, SECOND, room};

    fn uuids(rows: &[Row], order: &[usize]) -> Vec<String> {
        order
            .iter()
            .map(|&index| rows[index].uuid.clone().unwrap_or_default())
            .collect()
    }

    fn catalogue(uuid: &str) -> Vec<Row> {
        rows(&room().transact(), uuid).unwrap()
    }

    fn search(text: &str) -> Predicate {
        cocat_core::query::parse(text, &[]).unwrap()
    }

    #[test]
    fn all_events_are_the_projects_live_events() {
        let doc = room();
        let rows = all_rows(&doc.transact()).unwrap();
        let ids: Vec<_> = rows.iter().filter_map(|row| row.uuid.clone()).collect();
        assert_eq!(ids, ["e1", "e2", "e3"], "e4 is in the trash");
    }

    #[test]
    fn a_search_keeps_the_rows_a_smart_catalogue_would() {
        let doc = room();
        let txn = doc.transact();
        let rows = rows(&txn, "c-static").unwrap();
        let kept = matching(&txn, &rows, &search("confidence:>=0.9")).unwrap();
        assert_eq!(uuids(&rows, &kept), ["e3"]);
    }

    #[test]
    fn a_preview_holds_the_picked_events_and_what_the_search_finds() {
        let doc = room();
        let txn = doc.transact();
        let ids = |rows: Vec<Row>| -> Vec<String> {
            rows.into_iter().filter_map(|row| row.uuid).collect()
        };
        let found = preview_rows(&txn, "c-static", Some(&search("status:preliminary"))).unwrap();
        assert_eq!(ids(found), ["e1", "e2", "e3"], "e4 is picked but deleted");
        let picked = preview_rows(&txn, "c-static", None).unwrap();
        assert_eq!(ids(picked), ["e1", "e3"]);
    }

    #[test]
    fn a_search_finds_live_events_in_all_the_project_or_one_catalogue() {
        let doc = room();
        let txn = doc.transact();
        let ids = |rows: Vec<Row>| -> Vec<String> {
            rows.into_iter().filter_map(|row| row.uuid).collect()
        };
        let everywhere = search_rows(&txn, &search("tag:sw"), None).unwrap();
        assert_eq!(ids(everywhere), ["e1", "e2", "e3"], "e4 is deleted");
        let within = search_rows(&txn, &search("tag:sw"), Some("c-static")).unwrap();
        assert_eq!(ids(within), ["e1", "e3"]);
    }

    #[test]
    fn a_static_catalogue_lists_its_live_events_in_time_order() {
        let rows = catalogue("c-static");
        assert_eq!(uuids(&rows, &[0, 1]), ["e1", "e3"]);
        assert_eq!(
            rows[0],
            Row {
                uuid: Some("e1".into()),
                start: DAY0 + (14 * 3600 + 2 * 60 + 11) * SECOND,
                stop: DAY0 + (14 * 3600 + 2 * 60 + 52) * SECOND,
                tags: vec!["sheath".into(), "sw".into()],
                confidence: Some(0.82),
                status: Status::Confirmed,
                author: Some("alexis.jeandet".into()),
                attributes: json!({"spacecraft": "MMS1", "amplitude": 2.4})
                    .as_object()
                    .cloned()
                    .unwrap(),
                sources: Vec::new(),
            }
        );
    }

    #[test]
    fn an_event_without_an_author_shows_what_made_it() {
        let rows = catalogue("c-smart");
        assert_eq!(rows[1].uuid.as_deref(), Some("e2"));
        assert_eq!(rows[1].author.as_deref(), Some("sw-finder"));
    }

    #[test]
    fn derived_intervals_are_rows_without_a_uuid_with_their_sources() {
        let rows = catalogue("c-merged");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].uuid, None);
        assert_eq!(rows[0].sources, ["e1", "e2"]);
        assert_eq!(rows[0].start, DAY0 + (14 * 3600 + 2 * 60 + 11) * SECOND);
        assert_eq!(rows[1].sources, ["e3"]);
    }

    #[test]
    fn a_rule_that_fails_gives_the_core_error() {
        let error = rows(&room().transact(), "c-loop").unwrap_err();
        assert_eq!(
            error.to_string(),
            "the rule reaches catalogue c-loop again through itself"
        );
    }

    #[test]
    fn an_unknown_catalogue_has_no_rows() {
        assert!(catalogue("nope").is_empty());
    }

    #[test]
    fn one_event_reads_as_the_row_a_list_shows() {
        let doc = room();
        let one = event(&doc.transact(), "e2").unwrap();
        assert_eq!(one, catalogue("c-smart")[1]);
        assert_eq!(event(&doc.transact(), "nope"), None);
    }

    #[test]
    fn the_trash_lists_the_trashed_events() {
        let rows = trash_rows(&room().transact());
        assert_eq!(uuids(&rows, &[0]), ["e4"]);
        assert_eq!(rows.len(), 1);
    }

    fn filtered(query: &str) -> Vec<String> {
        let rows = catalogue("c-smart");
        uuids(&rows, &filter(&rows, query))
    }

    #[test]
    fn the_filter_matches_tags_status_author_and_attribute_values() {
        assert_eq!(filtered("burst"), ["e3"]);
        assert_eq!(filtered("preliminary"), ["e2"]);
        assert_eq!(filtered("lavraud"), ["e3"]);
        assert_eq!(filtered("mms2"), ["e2"]);
        assert_eq!(filtered("SW"), ["e1", "e2", "e3"]);
    }

    #[test]
    fn every_word_of_the_filter_must_match() {
        assert_eq!(filtered("sw mms1"), ["e1"]);
        assert!(filtered("burst mms1").is_empty());
    }

    #[test]
    fn the_filter_matches_the_start_by_its_prefix() {
        assert_eq!(filtered("2019-11-05"), ["e2"]);
        assert_eq!(filtered("2019-11-03 14:02"), ["e1"]);
        assert!(filtered("14:02").is_empty());
    }

    #[test]
    fn an_empty_filter_keeps_everything() {
        assert_eq!(filtered("  "), ["e1", "e2", "e3"]);
    }

    fn sorted(key: SortKey, descending: bool) -> Vec<String> {
        let rows = catalogue("c-smart");
        let mut order = filter(&rows, "");
        sort(&rows, &mut order, &key, descending);
        uuids(&rows, &order)
    }

    #[test]
    fn rows_sort_by_any_column_both_ways() {
        assert_eq!(sorted(SortKey::Duration, true), ["e3", "e1", "e2"]);
        assert_eq!(sorted(SortKey::Confidence, false), ["e2", "e1", "e3"]);
        assert_eq!(sorted(SortKey::Author, false), ["e1", "e3", "e2"]);
        assert_eq!(sorted(SortKey::Status, false), ["e2", "e1", "e3"]);
        assert_eq!(sorted(SortKey::Start, true), ["e3", "e2", "e1"]);
    }

    #[test]
    fn rows_without_the_value_sort_last_both_ways() {
        let key = || SortKey::Attribute("spacecraft".into());
        assert_eq!(sorted(key(), false), ["e1", "e2", "e3"]);
        assert_eq!(sorted(key(), true), ["e2", "e1", "e3"]);
    }

    #[test]
    fn the_attribute_columns_are_every_key_the_rows_hold() {
        let rows = catalogue("c-smart");
        assert_eq!(attribute_keys(&rows), ["amplitude", "region", "spacecraft"]);
    }
}
