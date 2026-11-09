//! Searching events with a predicate, without a smart catalogue: the events of the room
//! that match, or a test of one table row at a time.

use yrs::ReadTxn;

use super::EventRecord;
use super::members::{Evaluator, item_field};
use super::read::event;
use crate::rules::intervals::{Item, Source};
use crate::rules::{self, Lookups, Predicate, RuleError};
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
