//! The members of a catalogue: the events it lists plus what its rule yields (design
//! section 3), evaluated on every read as a pure function of the document.

use std::collections::HashSet;

use serde_json::Value;
use tracing::warn;
use yrs::{Map as _, MapRef, Out, ReadTxn};

use super::read::{entries, event, is_deleted, members, object, status};
use super::values::{get, integer, json};
use super::{CATALOGUES, EVENTS, EventRecord};
use crate::rules::intervals::{self, Item, Source, seconds};
use crate::rules::{self, Lookups, RuleError, Set};
use crate::time::Timestamp;

#[derive(Debug, Clone, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "most members are events: boxing them would allocate once per event for nothing"
)]
pub enum Member {
    /// An event of the room; `gap` is the distance in seconds `closest` measured.
    Event {
        record: EventRecord,
        gap: Option<f64>,
    },
    /// A read-only interval an interval operator made, with the events it came from.
    Derived {
        start: Timestamp,
        stop: Timestamp,
        sources: Vec<String>,
    },
}

/// The members of a catalogue in time order, or `None` when the room holds no such
/// catalogue.
pub fn catalogue_members<T: ReadTxn>(
    txn: &T,
    uuid: &str,
) -> Option<Result<Vec<Member>, RuleError>> {
    let catalogue = object(txn, CATALOGUES, uuid)?;
    let items = Evaluator::new(txn).own(uuid, &catalogue);
    Some(items.map(|items| items.iter().filter_map(|item| member(txn, item)).collect()))
}

/// The members of a catalogue that are events, in time order.
pub fn catalogue_events<T: ReadTxn>(
    txn: &T,
    uuid: &str,
) -> Option<Result<Vec<EventRecord>, RuleError>> {
    let events = |members: Vec<Member>| {
        members
            .into_iter()
            .filter_map(|member| match member {
                Member::Event { record, .. } => Some(record),
                Member::Derived { .. } => None,
            })
            .collect()
    };
    catalogue_members(txn, uuid).map(|members| members.map(events))
}

/// The rule of a catalogue, unparsed.
pub(super) fn rule_of<T: ReadTxn>(txn: &T, catalogue: &MapRef) -> Option<Value> {
    get(catalogue, txn, "rule").as_ref().and_then(json)
}

struct Evaluator<'t, T> {
    txn: &'t T,
    /// The catalogues being evaluated, outermost first, to tell a cycle.
    path: Vec<String>,
}

impl<'t, T: ReadTxn> Evaluator<'t, T> {
    fn new(txn: &'t T) -> Self {
        Self {
            txn,
            path: Vec::new(),
        }
    }

    /// A catalogue's listed events plus its rule's result.
    fn own(&mut self, uuid: &str, catalogue: &MapRef) -> Result<Vec<Item>, RuleError> {
        self.path.push(uuid.to_owned());
        let listed = listed(self.txn, catalogue);
        let ruled = match rule_of(self.txn, catalogue) {
            Some(rule) => self.set(&rules::parse(&rule)?)?,
            None => Vec::new(),
        };
        self.path.pop();
        Ok(intervals::union(&[listed, ruled]))
    }

    /// The members of a catalogue a rule names. One that is missing or in the trash is
    /// empty, so deleting a catalogue does not break the rules that use it.
    fn catalogue(&mut self, uuid: &str) -> Result<Vec<Item>, RuleError> {
        if self.path.iter().any(|seen| seen == uuid) {
            return Err(RuleError::Cycle(uuid.to_owned()));
        }
        match object(self.txn, CATALOGUES, uuid) {
            Some(catalogue) if !is_deleted(self.txn, &catalogue) => self.own(uuid, &catalogue),
            _ => Ok(Vec::new()),
        }
    }

    fn set(&mut self, set: &Set) -> Result<Vec<Item>, RuleError> {
        Ok(match set {
            Set::All { .. } => all(self.txn),
            Set::Catalogue { catalogue } => self.catalogue(catalogue)?,
            Set::Filter { filter, predicate } => match filter.as_ref() {
                Set::All { .. } => self.filter_all(predicate)?,
                _ => {
                    let items = self.set(filter)?;
                    self.filter(items, predicate)?
                }
            },
            Set::Merge { merge, gap } => intervals::merge(&self.set(merge)?, seconds(*gap)),
            Set::Intersect { intersect: (a, b) } => {
                intervals::intersect(&self.set(a)?, &self.set(b)?)
            }
            Set::Subtract { subtract: (a, b) } => intervals::subtract(&self.set(a)?, &self.set(b)?),
            Set::Union { union } => {
                let sets = union
                    .iter()
                    .map(|set| self.set(set))
                    .collect::<Result<Vec<_>, _>>()?;
                intervals::union(&sets)
            }
            Set::Shift { shift, by } => intervals::shift(&self.set(shift)?, seconds(*by)),
            Set::Tile { tile, width } => intervals::tile(&self.set(tile)?, seconds(*width))?,
            Set::Window {
                window: (a, b),
                within,
            } => intervals::window(&self.set(a)?, &self.set(b)?, seconds(*within)),
            Set::Closest { closest: (a, b) } => intervals::closest(&self.set(a)?, &self.set(b)?),
        })
    }

    fn filter(
        &mut self,
        items: Vec<Item>,
        predicate: &rules::Predicate,
    ) -> Result<Vec<Item>, RuleError> {
        let lookups = self.lookups(predicate)?;
        let txn = self.txn;
        let kept = |item: &Item| {
            let field = |name: &str| item_field(txn, item, name);
            rules::test(predicate, event_of(item), &field, &lookups)
        };
        Ok(items.into_iter().filter(kept).collect())
    }

    /// Filters every event in the scan that reads them, so each event map is looked up
    /// once: the common rule, and the one the 100 ms target is about.
    fn filter_all(&mut self, predicate: &rules::Predicate) -> Result<Vec<Item>, RuleError> {
        let lookups = self.lookups(predicate)?;
        let txn = self.txn;
        let Some(events) = txn.get_map(EVENTS) else {
            return Ok(Vec::new());
        };
        let kept = events.iter(txn).filter_map(|(uuid, value)| {
            let Out::YMap(map) = value else { return None };
            let field = |name: &str| event_field(txn, uuid, &map, name);
            let kept =
                !is_deleted(txn, &map) && rules::test(predicate, Some(uuid), &field, &lookups);
            kept.then(|| range(txn, uuid.to_owned(), &map)).flatten()
        });
        Ok(kept.collect())
    }

    fn lookups(&mut self, predicate: &rules::Predicate) -> Result<Lookups, RuleError> {
        Lookups::new(predicate, &mut |catalogue| self.events_of(catalogue))
    }

    /// The events among a catalogue's members; for `None`, the events some catalogue not
    /// in the trash lists.
    fn events_of(&mut self, catalogue: Option<&str>) -> Result<HashSet<String>, RuleError> {
        let Some(uuid) = catalogue else {
            return Ok(listed_anywhere(self.txn));
        };
        let items = self.catalogue(uuid)?;
        Ok(items
            .into_iter()
            .filter_map(|item| match item.source {
                Source::Event(uuid) => Some(uuid),
                Source::Derived(_) => None,
            })
            .collect())
    }
}

/// Every event not in the trash.
fn all<T: ReadTxn>(txn: &T) -> Vec<Item> {
    entries(txn, EVENTS)
        .into_iter()
        .filter(|(_, map)| !is_deleted(txn, map))
        .filter_map(|(uuid, map)| range(txn, uuid, &map))
        .collect()
}

/// The events a catalogue lists that exist and are not in the trash.
fn listed<T: ReadTxn>(txn: &T, catalogue: &MapRef) -> Vec<Item> {
    members(txn, catalogue)
        .into_iter()
        .filter_map(|uuid| {
            let map = object(txn, EVENTS, &uuid)?;
            if is_deleted(txn, &map) {
                return None;
            }
            let item = range(txn, uuid.clone(), &map);
            if item.is_none() {
                warn!(event = %uuid, "skipping a malformed event");
            }
            item
        })
        .collect()
}

fn listed_anywhere<T: ReadTxn>(txn: &T) -> HashSet<String> {
    entries(txn, CATALOGUES)
        .into_iter()
        .filter(|(_, catalogue)| !is_deleted(txn, catalogue))
        .flat_map(|(_, catalogue)| members(txn, &catalogue))
        .collect()
}

/// An event as an item of its range, or `None` when the range does not read.
fn range<T: ReadTxn>(txn: &T, uuid: String, map: &MapRef) -> Option<Item> {
    let start = get(map, txn, "start").as_ref().and_then(integer)?;
    let stop = get(map, txn, "stop").as_ref().and_then(integer)?;
    Some(Item::event(uuid, start, stop))
}

fn event_of(item: &Item) -> Option<&str> {
    match &item.source {
        Source::Event(uuid) => Some(uuid),
        Source::Derived(_) => None,
    }
}

/// A field of an item as a predicate sees it. A derived interval has only its range.
fn item_field<T: ReadTxn>(txn: &T, item: &Item, name: &str) -> Option<Value> {
    match (&item.source, name) {
        (Source::Event(uuid), _) => event_field(txn, uuid, &object(txn, EVENTS, uuid)?, name),
        (Source::Derived(_), "start") => Some(item.start.into()),
        (Source::Derived(_), "stop") => Some(item.stop.into()),
        (Source::Derived(_), _) => None,
    }
}

/// A field of an event as a predicate sees it: times as integer nanoseconds, the status by
/// name (`confirmed` when unset), attributes under their `#key`.
fn event_field<T: ReadTxn>(txn: &T, uuid: &str, map: &MapRef, name: &str) -> Option<Value> {
    let stored = get(map, txn, name);
    match name {
        "uuid" => Some(uuid.into()),
        "status" => Some(
            stored
                .as_ref()
                .map(status)
                .unwrap_or_default()
                .name()
                .into(),
        ),
        "start" | "stop" | "created" => stored.as_ref().and_then(integer).map(Value::from),
        _ => stored.as_ref().and_then(json),
    }
}

fn member<T: ReadTxn>(txn: &T, item: &Item) -> Option<Member> {
    match &item.source {
        Source::Event(uuid) => Some(Member::Event {
            record: event(txn, uuid)?,
            gap: item.gap.map(|gap| gap as f64 / 1e9),
        }),
        Source::Derived(sources) => Some(Member::Derived {
            start: Timestamp::from_unix_nanos(item.start)?,
            stop: Timestamp::from_unix_nanos(item.stop)?,
            sources: sources.clone(),
        }),
    }
}
