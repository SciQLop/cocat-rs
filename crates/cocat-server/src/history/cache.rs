//! Each room's versions as last grouped, so the history and the home page do not replay the
//! whole log on every request.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use bytes::Bytes;

use super::stamps::Stamp;
use super::versions::{Saved, Version, versions};

/// simplify: an entry stays until its room is deleted, so memory grows with every room
/// whose history was asked for (its versions and one stamp per record). Upgrade path: an
/// LRU bounded by entries or bytes (the `lru` crate) when that shows in the memory budget.
#[derive(Debug, Clone, Default)]
pub struct VersionCache(Arc<Mutex<HashMap<String, Entry>>>);

/// What the versions were grouped from. The log only grows, so a log with as many records,
/// the same stamps and the same saved versions groups the same.
#[derive(Debug)]
struct Entry {
    records: usize,
    slots: Vec<Option<Stamp>>,
    saved: Saved,
    versions: Vec<Version>,
}

impl VersionCache {
    /// The versions `versions` gives for the log, replayed only when the log changed since
    /// the last call for this room.
    pub fn versions(
        &self,
        room_id: &str,
        records: &[Bytes],
        slots: &[Option<Stamp>],
        saved: &Saved,
    ) -> Vec<Version> {
        if let Some(hit) = self.hit(room_id, records.len(), slots, saved) {
            return hit;
        }
        let fresh = versions(records, slots, saved);
        let entry = Entry {
            records: records.len(),
            slots: slots.to_vec(),
            saved: saved.clone(),
            versions: fresh.clone(),
        };
        self.lock().insert(room_id.to_owned(), entry);
        fresh
    }

    pub fn forget(&self, room_id: &str) {
        self.lock().remove(room_id);
    }

    fn hit(
        &self,
        room_id: &str,
        records: usize,
        slots: &[Option<Stamp>],
        saved: &Saved,
    ) -> Option<Vec<Version>> {
        let entries = self.lock();
        let entry = entries.get(room_id)?;
        let same = entry.records == records && entry.slots == slots && entry.saved == *saved;
        same.then(|| entry.versions.clone())
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Entry>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use cocat_core::model::{self, EventRecord};
    use cocat_core::time::Timestamp;
    use uuid::Uuid;
    use yrs::{Doc, ReadTxn, StateVector, Transact};

    use super::super::stamps::stamp_for;
    use super::*;

    const MINUTE: i64 = 60_000;

    /// A log of one event per record, each by one author `gap_ms` after the last.
    fn log(events: usize, gap_ms: i64) -> (Vec<Bytes>, Vec<Option<Stamp>>) {
        let author = Uuid::new_v4();
        (0..events)
            .map(|index| {
                let doc = Doc::new();
                let at = Timestamp::parse("2020-01-01T00:00:00Z").unwrap();
                let event = EventRecord::new(format!("e{index}"), at, at);
                model::create_event(&mut doc.transact_mut(), &event).unwrap();
                let record = Bytes::from(
                    doc.transact()
                        .encode_state_as_update_v1(&StateVector::default()),
                );
                let at_ms = index as i64 * gap_ms;
                let stamp = stamp_for(index as u64, Some(author), at_ms, &record);
                (record, Some(stamp))
            })
            .unzip()
    }

    #[test]
    fn a_cached_answer_equals_a_fresh_replay() {
        let (records, slots) = log(3, 10 * MINUTE);
        let saved = Saved::from([1]);
        let cache = VersionCache::default();

        let first = cache.versions("r", &records, &slots, &saved);
        let second = cache.versions("r", &records, &slots, &saved);

        assert_eq!(first, versions(&records, &slots, &saved));
        assert_eq!(second, first);
    }

    #[test]
    fn new_records_refresh_the_answer() {
        let (records, slots) = log(3, 10 * MINUTE);
        let cache = VersionCache::default();
        let saved = Saved::new();
        cache.versions("r", &records[..2], &slots[..2], &saved);

        let grown = cache.versions("r", &records, &slots, &saved);

        assert_eq!(grown, versions(&records, &slots, &saved));
        assert_eq!(grown.len(), 3);
    }

    #[test]
    fn a_new_saved_version_refreshes_the_answer() {
        let (records, slots) = log(2, MINUTE);
        let cache = VersionCache::default();
        assert_eq!(
            cache.versions("r", &records, &slots, &Saved::new()).len(),
            1
        );

        let split = cache.versions("r", &records, &slots, &Saved::from([0]));

        assert_eq!(split.len(), 2);
    }
}
