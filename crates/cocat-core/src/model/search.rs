//! Searching events with a predicate, without a smart catalogue: the events of the room
//! that match, or a test of one table row at a time.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use serde_json::{Map, Value};
use yrs::{Any, Map as _, Out, ReadTxn};

use super::members::{Evaluator, event_field, item_field};
use super::read::{add_text, catalogue_names, event, is_deleted, live};
use super::registry::attribute_types;
use super::values::{get, time};
use super::{ATTRIBUTE_PREFIX, AttributeType, CATALOGUES, EVENTS, EventRecord, Status, origin};
use crate::query::search_box::{Shown, Vocabulary, Words};
use crate::rules::intervals::{Item, Source};
use crate::rules::{self, ANY_TEXT, Lookups, Predicate, RuleError, Set};
use crate::time::Timestamp;

/// The events not in the trash that match `predicate` at `now`, in time order.
pub fn search<T: ReadTxn>(
    txn: &T,
    predicate: &Predicate,
    now: Timestamp,
) -> Result<Vec<EventRecord>, RuleError> {
    let mut items = Evaluator::new(txn, now).filter_all(predicate)?;
    items.sort();
    Ok(items
        .iter()
        .filter_map(|item| match &item.source {
            Source::Event(uuid) => event(txn, uuid),
            Source::Derived(_) => None,
        })
        .collect())
}

/// A predicate ready to test items one by one: its lookups are read once.
#[derive(Debug)]
pub struct Search {
    predicate: Predicate,
    lookups: Lookups,
}

impl Search {
    pub fn new<T: ReadTxn>(
        txn: &T,
        predicate: &Predicate,
        now: Timestamp,
    ) -> Result<Self, RuleError> {
        Ok(Self {
            predicate: predicate.clone(),
            lookups: Evaluator::new(txn, now).lookups(predicate)?,
        })
    }

    /// Whether the event `uuid` matches, or, for `None`, the derived interval
    /// `[start, stop)` (nanoseconds); an event's range is read from the room.
    pub fn matches<T: ReadTxn>(&self, txn: &T, uuid: Option<&str>, start: i64, stop: i64) -> bool {
        let item = match uuid {
            Some(uuid) => Item::event(uuid, start, stop),
            None => Item::derived(start, stop, Vec::new()),
        };
        let field = |name: &str| item_field(txn, &item, name);
        rules::test(&self.predicate, uuid, &field, &self.lookups)
    }
}

/// One row of a table to test: an event of the room, or an interval a rule derived, which
/// has only its range (nanoseconds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    Event(String),
    Interval { start: i64, stop: i64 },
}

impl Search {
    /// Whether each probe matches, as a smart catalogue over all events would hold it: an
    /// event in the trash or not in the room never does. Each event map is looked up once.
    pub fn mask<T: ReadTxn>(&self, txn: &T, probes: &[Probe]) -> Vec<bool> {
        let events = txn.get_map(EVENTS);
        let event_matches = |uuid: &str| {
            let Some(Out::YMap(map)) = events.as_ref().and_then(|events| events.get(txn, uuid))
            else {
                return false;
            };
            let field = |name: &str| event_field(txn, uuid, &map, name);
            !is_deleted(txn, &map)
                && rules::test(&self.predicate, Some(uuid), &field, &self.lookups)
        };
        probes
            .iter()
            .map(|probe| match probe {
                Probe::Event(uuid) => event_matches(uuid),
                Probe::Interval { start, stop } => self.matches(txn, None, *start, *stop),
            })
            .collect()
    }
}

/// Whether each probe matches `predicate` at `now`, as a smart catalogue with this search
/// would hold it: over all events, or among the members of catalogue `source` (one missing
/// or in the trash holds nothing).
pub fn search_mask<T: ReadTxn>(
    txn: &T,
    predicate: &Predicate,
    source: Option<&str>,
    now: Timestamp,
    probes: &[Probe],
) -> Result<Vec<bool>, RuleError> {
    let mut mask = Search::new(txn, predicate, now)?.mask(txn, probes);
    if let Some(source) = source {
        let catalogue = Set::Catalogue {
            catalogue: source.to_owned(),
        };
        let items = Evaluator::new(txn, now).set(&catalogue)?;
        let mut events = HashSet::new();
        let mut intervals = HashSet::new();
        for item in &items {
            match &item.source {
                Source::Event(uuid) => events.insert(uuid.as_str()),
                Source::Derived(_) => intervals.insert((item.start, item.stop)),
            };
        }
        for (kept, probe) in mask.iter_mut().zip(probes) {
            *kept &= match probe {
                Probe::Event(uuid) => events.contains(uuid.as_str()),
                Probe::Interval { start, stop } => intervals.contains(&(*start, *stop)),
            };
        }
    }
    Ok(mask)
}

/// Whether each probe passes the table's plain-words filter ([`Words`]), read as the table
/// shows it: an event in the trash too; its author, else the name of what made it.
pub fn words_mask<T: ReadTxn>(txn: &T, text: &str, probes: &[Probe]) -> Vec<bool> {
    let words = Words::new(text);
    let mut origins: HashMap<String, Option<String>> = HashMap::new();
    let mut origin_name = |id: &str| {
        origins
            .entry(id.to_owned())
            .or_insert_with(|| origin(txn, id).map(|found| found.name))
            .clone()
    };
    let nothing = Map::new();
    probes
        .iter()
        .map(|probe| match probe {
            Probe::Event(uuid) => event(txn, uuid).is_some_and(|record| {
                let author = record
                    .author
                    .clone()
                    .or_else(|| origin_name(record.origin.as_deref()?));
                words.matches(&Shown {
                    start: record.start.unix_nanos().unwrap_or_default(),
                    tags: &record.tags,
                    status: record.status,
                    author: author.as_deref(),
                    attributes: &record.attributes,
                })
            }),
            Probe::Interval { start, .. } => words.matches(&Shown {
                start: *start,
                tags: &[],
                status: Status::default(),
                author: None,
                attributes: &nothing,
            }),
        })
        .collect()
}

/// What the room holds that a search can name, for completions: tags, the authors of live
/// events, catalogue names and attribute keys, each sorted.
pub fn vocabulary<T: ReadTxn>(txn: &T) -> Vocabulary {
    let catalogues: BTreeSet<String> = catalogue_names(txn)
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    let types = attribute_types(txn);
    let texts: Vec<String> = types
        .iter()
        .filter(|(_, info)| info.kind == AttributeType::Text)
        .map(|(key, _)| format!("{ATTRIBUTE_PREFIX}{key}"))
        .collect();
    let mut words = event_words(txn, &texts);
    for catalogue in live(txn, CATALOGUES) {
        if let Some(Any::Array(tags)) = get(&catalogue, txn, "tags") {
            tags.iter().for_each(|tag| add_text(&mut words.tags, tag));
        }
    }
    let attribute_values = texts
        .iter()
        .zip(words.values)
        .filter(|(_, counted)| !counted.is_empty())
        .map(|(field, counted)| {
            (
                field[ATTRIBUTE_PREFIX.len_utf8()..].to_owned(),
                most_frequent(counted),
            )
        })
        .collect();
    Vocabulary {
        tags: words.tags.into_iter().collect(),
        authors: words.authors.into_iter().collect(),
        catalogues: catalogues.into_iter().collect(),
        attributes: types.into_keys().collect(),
        attribute_values,
    }
}

/// How many values of one text attribute the vocabulary keeps, the most frequent: enough for
/// any list a person picks from, few enough that a free-text attribute (a comment, an id)
/// does not carry its every value to the completions.
pub const ATTRIBUTE_VALUES_KEPT: usize = 200;

/// What the events outside the trash hold that a search names.
struct EventWords {
    tags: BTreeSet<String>,
    /// Those of the events `search` reads: with a start and a stop.
    authors: BTreeSet<String>,
    /// How often each value comes, for each of the text attribute `fields` asked for.
    values: Vec<HashMap<Arc<str>, usize>>,
}

/// One walk over each event's fields: a project holds many events, and looking a field up
/// by name costs about as much as walking them all, once per field asked for.
fn event_words<T: ReadTxn>(txn: &T, fields: &[String]) -> EventWords {
    let column: HashMap<&str, usize> = fields
        .iter()
        .enumerate()
        .map(|(at, field)| (field.as_str(), at))
        .collect();
    let mut words = EventWords {
        tags: BTreeSet::new(),
        authors: BTreeSet::new(),
        values: vec![HashMap::new(); fields.len()],
    };
    for event in live(txn, EVENTS) {
        let (mut author, mut times) = (None, 0);
        for (key, value) in event.iter(txn) {
            let Out::Any(any) = value else { continue };
            match (key, any) {
                ("tags", Any::Array(tags)) => {
                    tags.iter().for_each(|tag| add_text(&mut words.tags, tag));
                }
                ("author", Any::String(name)) => author = Some(name),
                ("start" | "stop", any) => times += usize::from(time(&any).is_some()),
                (key, Any::String(value)) => {
                    if let Some(&at) = column.get(key) {
                        *words.values[at].entry(value).or_default() += 1;
                    }
                }
                _ => {}
            }
        }
        if let Some(author) = author
            && times == 2
            && !words.authors.contains(author.as_ref())
        {
            words.authors.insert(author.to_string());
        }
    }
    words
}

fn most_frequent(counts: HashMap<Arc<str>, usize>) -> Vec<String> {
    let mut ranked: Vec<(Arc<str>, usize)> = counts.into_iter().collect();
    ranked.sort_by(|(left, many), (right, more)| more.cmp(many).then_with(|| left.cmp(right)));
    ranked
        .into_iter()
        .take(ATTRIBUTE_VALUES_KEPT)
        .map(|(value, _)| value.to_string())
        .collect()
}

/// Whether an event read out of its room matches, as it would in the room. A term naming a
/// catalogue asks `lookups`, which knows no room: give it the answer beforehand.
pub fn record_matches(predicate: &Predicate, lookups: &Lookups, record: &EventRecord) -> bool {
    let field = |name: &str| record_field(record, name);
    rules::test(predicate, Some(&record.uuid), &field, lookups)
}

/// A field of a record as a predicate reads it in the room (see `members::event_field`).
fn record_field(record: &EventRecord, name: &str) -> Option<Value> {
    let time = |at: crate::time::Timestamp| at.unix_nanos().map(Value::from);
    let text = |text: &Option<String>| text.clone().map(Value::from);
    let number = |number: Option<f64>| number.map(Value::from);
    let list = |items: &[String]| (!items.is_empty()).then(|| Value::from(items.to_vec()));
    match name {
        "uuid" => Some(record.uuid.clone().into()),
        "start" => time(record.start),
        "stop" => time(record.stop),
        "created" => record.created.and_then(time),
        "author" => text(&record.author),
        "origin" => text(&record.origin),
        "source_id" => text(&record.source_id),
        "status" => Some(record.status.name().into()),
        "confidence" => number(record.confidence),
        "start_err" => number(record.start_err),
        "stop_err" => number(record.stop_err),
        "tags" => list(&record.tags),
        "products" => list(&record.products),
        "shape" => record.shape.as_ref().map(|_| Value::Bool(true)),
        ANY_TEXT => Some(any_text(record)),
        _ => name
            .strip_prefix(ATTRIBUTE_PREFIX)
            .and_then(|key| record.attributes.get(key).cloned()),
    }
}

/// The texts `ANY_TEXT` names (see `members::any_text`).
fn any_text(record: &EventRecord) -> Value {
    let attributes = record.attributes.values().map(|value| match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    });
    let named = [&record.author, &record.source_id, &record.origin];
    record
        .tags
        .iter()
        .cloned()
        .chain(named.into_iter().flatten().cloned())
        .chain(attributes)
        .chain([record.status.name().to_owned()])
        .map(Value::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use yrs::{Doc, Transact};

    use super::*;
    use crate::model::{Status, Vertex, create_event, members::item_field};
    use crate::time::Timestamp;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_unix_nanos(seconds * 1_000_000_000).unwrap()
    }

    fn full() -> EventRecord {
        let mut record = EventRecord::new("e1", at(10), at(70));
        record.author = Some("alice@lab.fr".into());
        record.created = Some(at(5));
        record.origin = Some("o1".into());
        record.status = Status::Reviewed;
        record.confidence = Some(0.8);
        record.start_err = Some(1.5);
        record.stop_err = Some(2.0);
        record.source_id = Some("S-1".into());
        record.tags = vec!["shock".into(), "bow".into()];
        record.products = vec!["amda/b".into()];
        record.attributes.insert("density".into(), json!(12.5));
        record.attributes.insert("spacecraft".into(), json!("MMS1"));
        record
    }

    fn shaped() -> EventRecord {
        let mut record = EventRecord::new("e2", at(0), at(0));
        record.shape = Some(vec![
            Vertex {
                time: at(1),
                value: 1.0,
            },
            Vertex {
                time: at(4),
                value: 2.0,
            },
        ]);
        record
    }

    /// Lists in one order, numbers as floats: the room may store 2.0 as 2.
    fn sorted(value: Option<Value>) -> Option<Value> {
        match value {
            Some(Value::Number(number)) => number.as_f64().map(Value::from),
            Some(Value::Array(mut items)) => {
                items.sort_by_key(|item| item.to_string());
                Some(Value::Array(items))
            }
            other => other,
        }
    }

    #[test]
    fn a_record_reads_its_fields_as_the_room_does() {
        let doc = Doc::new();
        let records = [full(), shaped(), EventRecord::new("e3", at(1), at(2))];
        {
            let mut txn = doc.transact_mut();
            for record in &records {
                create_event(&mut txn, record).unwrap();
            }
        }
        let txn = doc.transact();
        let names =
            rules::FIELDS
                .iter()
                .copied()
                .chain([ANY_TEXT, "#density", "#spacecraft", "#missing"]);
        for record in &records {
            let stored = crate::model::event(&txn, &record.uuid).unwrap();
            let item = crate::rules::Item::event(&record.uuid, 0, 0);
            for name in names.clone().filter(|name| *name != "duration") {
                let in_room = item_field(&txn, &item, name);
                let in_room = match name {
                    "shape" => in_room.map(|_| Value::Bool(true)),
                    _ => in_room,
                };
                assert_eq!(
                    sorted(record_field(&stored, name)),
                    sorted(in_room),
                    "{} {name}",
                    record.uuid
                );
            }
        }
    }
}
