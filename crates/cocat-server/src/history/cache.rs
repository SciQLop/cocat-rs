//! Each room's versions as last grouped, so the history and the home page do not replay the
//! whole log on every request.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;

use super::diff::Analysis;
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

const DIFF_CHANGES: usize = 300_000;
const DIFF_LIFETIME: Duration = Duration::from_secs(5 * 60);

/// The room, the records the diff starts and ends at, and the catalogue it is narrowed to.
/// The log only grows, so the same records give the same diff; the version still open ends
/// at a later record after new edits and so finds no entry.
pub type DiffKey = (String, usize, usize, Option<String>);

type CachedDiff = (DiffKey, Instant, Arc<Analysis>);

/// The last few diffs the history computed, so the next pages of a version and a reopened
/// version do not replay the room again.
///
/// simplify: at most 300 000 event changes held (about 300 MB as JSON, a STIX version is
/// 107 000) for 5 minutes, the oldest entry out first, no hit refreshes an entry, and a diff
/// over the budget is never kept. The room memory budget does not see this cache. Upgrade
/// path: an LRU weighed in bytes (the `lru` crate), counted in the room budget.
#[derive(Debug, Clone)]
pub struct DiffCache {
    entries: Arc<Mutex<Vec<CachedDiff>>>,
    change_budget: usize,
    lifetime: Duration,
    computations: Arc<AtomicUsize>,
}

impl Default for DiffCache {
    fn default() -> Self {
        Self::new(DIFF_CHANGES, DIFF_LIFETIME)
    }
}

impl DiffCache {
    pub fn new(change_budget: usize, lifetime: Duration) -> Self {
        Self {
            entries: Arc::default(),
            change_budget,
            lifetime,
            computations: Arc::default(),
        }
    }

    /// The diff under `key`, computed by `compute` only when none is held. Two requests
    /// missing together both compute: the lock is not held meanwhile.
    pub fn get_or_compute(
        &self,
        key: DiffKey,
        compute: impl FnOnce() -> Analysis,
    ) -> Arc<Analysis> {
        if let Some(hit) = self.hit(&key) {
            return hit;
        }
        self.computations.fetch_add(1, Ordering::Relaxed);
        let fresh = Arc::new(compute());
        if changes(&fresh) > self.change_budget {
            return fresh;
        }
        let mut entries = self.lock();
        let lifetime = self.lifetime;
        entries.retain(|(held, born, _)| *held != key && born.elapsed() < lifetime);
        entries.push((key, Instant::now(), Arc::clone(&fresh)));
        while held_changes(&entries) > self.change_budget {
            entries.remove(0);
        }
        fresh
    }

    /// How many event changes the held diffs hold, for tests.
    #[cfg(test)]
    fn held_changes(&self) -> usize {
        held_changes(&self.lock())
    }

    /// How many diffs were computed, for tests to see a hit.
    pub fn computations(&self) -> usize {
        self.computations.load(Ordering::Relaxed)
    }

    pub fn forget(&self, room_id: &str) {
        self.lock().retain(|(key, _, _)| key.0 != room_id);
    }

    fn hit(&self, key: &DiffKey) -> Option<Arc<Analysis>> {
        let mut entries = self.lock();
        let lifetime = self.lifetime;
        entries.retain(|(_, born, _)| born.elapsed() < lifetime);
        let (_, _, analysis) = entries.iter().find(|(held, _, _)| held == key)?;
        Some(Arc::clone(analysis))
    }

    fn lock(&self) -> MutexGuard<'_, Vec<CachedDiff>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn changes(analysis: &Analysis) -> usize {
    analysis.diff.events.len()
}

fn held_changes(entries: &[CachedDiff]) -> usize {
    entries.iter().map(|(_, _, held)| changes(held)).sum()
}

#[cfg(test)]
mod tests {
    use cocat_core::model::{self, EventRecord};
    use cocat_core::time::Timestamp;
    use uuid::Uuid;
    use yrs::{Doc, ReadTxn, StateVector, Transact};

    use super::super::diff::{Change, EventChange};
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

    fn key(end: usize) -> DiffKey {
        ("r".to_owned(), 0, end, None)
    }

    #[test]
    fn a_held_diff_is_not_computed_again() {
        let cache = DiffCache::default();
        cache.get_or_compute(key(1), Analysis::default);
        cache.get_or_compute(key(1), || unreachable!("the diff is held"));
        assert_eq!(cache.computations(), 1);
    }

    fn analysis_of(changes: usize) -> Analysis {
        let event = || EventChange {
            uuid: String::new(),
            change: Change::Added,
            fields: Vec::new(),
            before: None,
            after: None,
        };
        let mut analysis = Analysis::default();
        analysis.diff.events = (0..changes).map(|_| event()).collect();
        analysis
    }

    #[test]
    fn the_cache_drops_old_diffs_to_stay_under_its_change_budget() {
        let cache = DiffCache::new(250, Duration::from_secs(60));
        for end in 1..=3 {
            cache.get_or_compute(key(end), || analysis_of(100));
        }
        cache.get_or_compute(key(3), || unreachable!("the newest diff is held"));
        cache.get_or_compute(key(2), || unreachable!("the second newest diff is held"));
        cache.get_or_compute(key(1), || analysis_of(100));
        assert_eq!(cache.computations(), 4);
    }

    #[test]
    fn a_diff_bigger_than_the_budget_is_not_kept() {
        let cache = DiffCache::new(250, Duration::from_secs(60));
        cache.get_or_compute(key(1), || analysis_of(100));
        cache.get_or_compute(key(2), || analysis_of(251));
        cache.get_or_compute(key(1), || unreachable!("the small diff stays"));
        cache.get_or_compute(key(2), || analysis_of(251));
        assert_eq!(cache.computations(), 3);
    }

    #[test]
    fn an_expired_diff_leaves_when_another_is_stored() {
        let cache = DiffCache::new(250, Duration::ZERO);
        cache.get_or_compute(key(1), || analysis_of(100));
        cache.get_or_compute(key(2), || analysis_of(100));
        assert_eq!(cache.held_changes(), 100);
    }

    #[test]
    fn an_expired_diff_is_computed_again() {
        let cache = DiffCache::new(2, Duration::ZERO);
        cache.get_or_compute(key(1), Analysis::default);
        cache.get_or_compute(key(1), Analysis::default);
        assert_eq!(cache.computations(), 2);
    }

    #[test]
    fn a_forgotten_room_loses_its_diffs() {
        let cache = DiffCache::default();
        cache.get_or_compute(key(1), Analysis::default);
        cache.forget("r");
        cache.get_or_compute(key(1), Analysis::default);
        assert_eq!(cache.computations(), 2);
    }
}
