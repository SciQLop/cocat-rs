//! Grouping a room's records into versions: runs of records by one author, each summed up
//! by what its records touched.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use bytes::Bytes;
use cocat_core::model::{self, CATALOGUES, EVENTS, ORIGINS, Origin, OriginKind, Status};
use serde::Serialize;
use uuid::Uuid;
use yrs::types::map::MapEvent;
use yrs::types::{EntryChange, Event, PathSegment};
use yrs::updates::decoder::Decode;
use yrs::{Any, DeepObservable, Doc, Map, Out, Transact, TransactionMut, Update};

use super::stamps::Stamp;

pub const VERSION_GAP_MS: i64 = 300_000;

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct Version {
    /// The version's last record.
    pub id: u64,
    pub first: u64,
    pub author: Option<Uuid>,
    pub started_ms: Option<i64>,
    pub ended_ms: Option<i64>,
    pub via: Option<String>,
    pub auto_name: Option<String>,
    pub events_added: u32,
    pub events_changed: u32,
    pub events_removed: u32,
    pub catalogues: Vec<String>,
    pub rules_changed: Vec<String>,
}

/// What one record changed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Touched {
    pub events_added: BTreeSet<String>,
    pub events_changed: BTreeSet<String>,
    pub events_removed: BTreeSet<String>,
    pub catalogues: BTreeSet<String>,
    pub rules: BTreeSet<String>,
    pub origins_added: Vec<Origin>,
    /// The names of the origins the record's new events reference.
    pub via: BTreeSet<String>,
}

impl Touched {
    fn extend(&mut self, other: Touched) {
        self.events_added.extend(other.events_added);
        self.events_changed.extend(other.events_changed);
        self.events_removed.extend(other.events_removed);
        self.catalogues.extend(other.catalogues);
        self.rules.extend(other.rules);
        self.origins_added.extend(other.origins_added);
        self.via.extend(other.via);
    }
}

/// What the observers gather while one record is applied.
#[derive(Default)]
struct Collected {
    touched: Touched,
    /// The origin ids the new events reference, named once the record is in.
    event_origins: BTreeSet<String>,
}

type Shared = Arc<Mutex<Collected>>;

/// One replay of the whole log; the observers only see what each record changed.
pub fn touched_by_records(records: &[Bytes]) -> Vec<Touched> {
    let doc = Doc::new();
    let collected = Shared::default();
    observe(&doc, EVENTS, &collected, collect_events);
    observe(&doc, CATALOGUES, &collected, collect_catalogues);
    observe(&doc, ORIGINS, &collected, collect_origins);
    records
        .iter()
        .map(|record| {
            apply(&doc, record);
            let Collected {
                mut touched,
                event_origins,
            } = std::mem::take(&mut *lock(&collected));
            touched.via = origin_names(&doc, &event_origins);
            touched
        })
        .collect()
}

/// Newest first. `stamps` holds one slot per record, as `valid_stamps` gives them.
pub fn versions(records: &[Bytes], stamps: &[Option<Stamp>]) -> Vec<Version> {
    let slots: Vec<Option<Stamp>> = (0..records.len())
        .map(|index| stamps.get(index).copied().flatten())
        .collect();
    let mut touched = touched_by_records(records).into_iter();
    let mut versions = Vec::new();
    let mut first = 0;
    for run in slots.chunk_by(continues) {
        versions.push(version(first, run, merge(touched.by_ref().take(run.len()))));
        first += run.len();
    }
    versions.reverse();
    versions
}

fn observe(
    doc: &Doc,
    root: &str,
    collected: &Shared,
    collect: fn(&TransactionMut, &MapEvent, &mut Collected),
) {
    let collected = Arc::clone(collected);
    doc.get_or_insert_map(root)
        .observe_deep(root, move |txn, events| {
            let mut collected = lock(&collected);
            for event in events.iter() {
                if let Event::Map(event) = event {
                    collect(txn, event, &mut collected);
                }
            }
        });
}

fn lock(shared: &Shared) -> MutexGuard<'_, Collected> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A record that does not decode or apply is skipped, as the room skips it on load.
fn apply(doc: &Doc, record: &[u8]) {
    if let Ok(update) = Update::decode_v1(record) {
        let _ = doc.transact_mut().apply_update(update);
    }
}

fn collect_events(txn: &TransactionMut, event: &MapEvent, collected: &mut Collected) {
    let Collected {
        touched,
        event_origins,
    } = collected;
    let keys = event.keys(txn);
    match event.path().front() {
        None => {
            for (uuid, change) in keys {
                let uuid = uuid.to_string();
                match change {
                    EntryChange::Inserted(value) => {
                        event_origins.extend(origin_id(txn, value));
                        touched.events_added.insert(uuid);
                    }
                    EntryChange::Updated(..) => {
                        touched.events_changed.insert(uuid);
                    }
                    EntryChange::Removed(_) => {
                        touched.events_removed.insert(uuid);
                    }
                }
            }
        }
        Some(PathSegment::Key(uuid)) => {
            let set = if became_deleted(keys.get("status")) {
                &mut touched.events_removed
            } else {
                &mut touched.events_changed
            };
            set.insert(uuid.to_string());
        }
        Some(PathSegment::Index(_)) => {}
    }
}

fn collect_catalogues(txn: &TransactionMut, event: &MapEvent, collected: &mut Collected) {
    let touched = &mut collected.touched;
    let keys = event.keys(txn);
    let path = event.path();
    match path.front() {
        None => touched
            .catalogues
            .extend(keys.keys().map(|uuid| uuid.to_string())),
        Some(PathSegment::Key(uuid)) => {
            if path.len() == 1 && keys.contains_key("rule") {
                touched.rules.insert(uuid.to_string());
            }
            touched.catalogues.insert(uuid.to_string());
        }
        Some(PathSegment::Index(_)) => {}
    }
}

fn collect_origins(txn: &TransactionMut, event: &MapEvent, collected: &mut Collected) {
    if !event.path().is_empty() {
        return;
    }
    for (id, change) in event.keys(txn) {
        if let EntryChange::Inserted(_) = change {
            collected
                .touched
                .origins_added
                .extend(model::origin(txn, id));
        }
    }
}

fn origin_id(txn: &TransactionMut, event: &Out) -> Option<String> {
    let Out::YMap(event) = event else {
        return None;
    };
    match event.get(txn, "origin") {
        Some(Out::Any(Any::String(id))) => Some(id.to_string()),
        _ => None,
    }
}

fn became_deleted(status: Option<&EntryChange>) -> bool {
    match status {
        Some(EntryChange::Inserted(Out::Any(Any::String(status))))
        | Some(EntryChange::Updated(_, Out::Any(Any::String(status)))) => {
            **status == *Status::Deleted.name()
        }
        _ => false,
    }
}

fn origin_names(doc: &Doc, ids: &BTreeSet<String>) -> BTreeSet<String> {
    let txn = doc.transact();
    ids.iter()
        .filter_map(|id| model::origin(&txn, id))
        .map(|origin| origin.name)
        .collect()
}

/// Whether the next record belongs to the same version as the one before it.
fn continues(previous: &Option<Stamp>, next: &Option<Stamp>) -> bool {
    match (previous, next) {
        (None, None) => true,
        (Some(previous), Some(next)) => {
            previous.user == next.user
                && next.at_ms.saturating_sub(previous.at_ms) <= VERSION_GAP_MS
        }
        _ => false,
    }
}

/// An event added or removed within the version counts as that, not as changed.
fn merge(records: impl Iterator<Item = Touched>) -> Touched {
    let mut merged = records.fold(Touched::default(), |mut merged, touched| {
        merged.extend(touched);
        merged
    });
    merged.events_changed.retain(|uuid| {
        !merged.events_added.contains(uuid) && !merged.events_removed.contains(uuid)
    });
    merged
}

fn version(first: usize, run: &[Option<Stamp>], touched: Touched) -> Version {
    let first_stamp = run.first().copied().flatten();
    let last_stamp = run.last().copied().flatten();
    let via = touched
        .via
        .first()
        .or_else(|| touched.origins_added.first().map(|origin| &origin.name))
        .cloned();
    Version {
        id: index(first + run.len().saturating_sub(1)),
        first: index(first),
        author: first_stamp.and_then(|stamp| stamp.user),
        started_ms: first_stamp.map(|stamp| stamp.at_ms),
        ended_ms: last_stamp.map(|stamp| stamp.at_ms),
        via,
        auto_name: touched.origins_added.iter().find_map(auto_name),
        events_added: count(&touched.events_added),
        events_changed: count(&touched.events_changed),
        events_removed: count(&touched.events_removed),
        catalogues: touched.catalogues.into_iter().collect(),
        rules_changed: touched.rules.into_iter().collect(),
    }
}

/// Imports and materializations name their version after their origin.
fn auto_name(origin: &Origin) -> Option<String> {
    let kind = match origin.kind {
        OriginKind::Import => "import",
        OriginKind::Rule => "rule",
        OriginKind::Detector | OriginKind::Model => return None,
    };
    Some(format!("{kind}: {}", origin.name))
}

fn index(index: usize) -> u64 {
    u64::try_from(index).unwrap_or(u64::MAX)
}

fn count(set: &BTreeSet<String>) -> u32 {
    u32::try_from(set.len()).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use cocat_core::model::{EventRecord, NewCatalogue};
    use cocat_core::time::Timestamp;
    use serde_json::json;
    use yrs::{Doc, ReadTxn, Transact, TransactionMut};

    use super::super::stamps::stamp_for;
    use super::*;

    const MINUTE: i64 = 60_000;

    /// A scratch room whose every transaction becomes one record.
    #[derive(Default)]
    struct Room {
        doc: Doc,
        records: Vec<Bytes>,
        stamps: Vec<Option<Stamp>>,
    }

    impl Room {
        fn write(&mut self, stamp: Option<(Uuid, i64)>, edit: impl FnOnce(&mut TransactionMut)) {
            let before = self.doc.transact().state_vector();
            let mut txn = self.doc.transact_mut();
            edit(&mut txn);
            let record = Bytes::from(txn.encode_diff_v1(&before));
            drop(txn);
            let index = self.records.len() as u64;
            self.stamps
                .push(stamp.map(|(user, at)| stamp_for(index, Some(user), at, &record)));
            self.records.push(record);
        }

        fn versions(&self) -> Vec<Version> {
            versions(&self.records, &self.stamps)
        }
    }

    fn user(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn event(uuid: &str) -> EventRecord {
        let at = |nanos| Timestamp::from_unix_nanos(nanos).unwrap();
        EventRecord::new(uuid, at(0), at(1_000))
    }

    fn create(uuid: &'static str) -> impl FnOnce(&mut TransactionMut) {
        move |txn| {
            model::create_event(txn, &event(uuid)).unwrap();
        }
    }

    fn set_confidence(uuid: &'static str) -> impl FnOnce(&mut TransactionMut) {
        move |txn| {
            let field = model::EventField::Confidence(Some(0.5));
            assert!(model::set_event_field(txn, uuid, &field).unwrap());
        }
    }

    fn ranges(versions: &[Version]) -> Vec<(u64, u64)> {
        versions.iter().map(|v| (v.first, v.id)).collect()
    }

    #[test]
    fn records_by_one_author_within_five_minutes_make_one_version() {
        let mut room = Room::default();
        room.write(Some((user(1), 0)), create("a"));
        room.write(Some((user(1), MINUTE)), create("b"));
        room.write(Some((user(1), 5 * MINUTE)), create("c"));

        let versions = room.versions();

        assert_eq!(ranges(&versions), [(0, 2)]);
        assert_eq!(versions[0].author, Some(user(1)));
        assert_eq!(versions[0].started_ms, Some(0));
        assert_eq!(versions[0].ended_ms, Some(5 * MINUTE));
        assert_eq!(versions[0].events_added, 3);
    }

    #[test]
    fn a_gap_over_five_minutes_starts_a_new_version() {
        let mut room = Room::default();
        room.write(Some((user(1), 0)), create("a"));
        room.write(Some((user(1), VERSION_GAP_MS + 1)), create("b"));

        assert_eq!(ranges(&room.versions()), [(1, 1), (0, 0)]);
    }

    #[test]
    fn another_author_starts_a_new_version() {
        let mut room = Room::default();
        room.write(Some((user(1), 0)), create("a"));
        room.write(Some((user(2), MINUTE)), create("b"));

        let versions = room.versions();

        assert_eq!(ranges(&versions), [(1, 1), (0, 0)]);
        assert_eq!(versions[0].author, Some(user(2)));
        assert_eq!(versions[1].author, Some(user(1)));
    }

    #[test]
    fn unstamped_records_group_into_one_version_before_history() {
        let mut room = Room::default();
        room.write(None, create("a"));
        room.write(None, create("b"));
        room.write(Some((user(1), 0)), create("c"));

        let versions = room.versions();

        assert_eq!(ranges(&versions), [(2, 2), (0, 1)]);
        let before_history = &versions[1];
        assert_eq!(before_history.author, None);
        assert_eq!(before_history.started_ms, None);
        assert_eq!(before_history.ended_ms, None);
        assert_eq!(before_history.events_added, 2);
    }

    #[test]
    fn counts_added_changed_and_removed_events() {
        let mut room = Room::default();
        room.write(None, create("old"));
        room.write(Some((user(1), 0)), create("a"));
        room.write(Some((user(1), 1)), create("b"));
        room.write(Some((user(1), 2)), |txn| {
            assert!(model::delete_event(txn, "old"));
        });
        room.write(Some((user(2), 3)), set_confidence("a"));

        let counts: Vec<_> = room
            .versions()
            .iter()
            .map(|v| (v.events_added, v.events_changed, v.events_removed))
            .collect();

        assert_eq!(counts, [(0, 1, 0), (2, 0, 1), (1, 0, 0)]);
    }

    #[test]
    fn an_event_added_and_changed_in_one_version_counts_as_added() {
        let mut room = Room::default();
        room.write(Some((user(1), 0)), create("a"));
        room.write(Some((user(1), 1)), set_confidence("a"));

        let version = &room.versions()[0];

        assert_eq!((version.events_added, version.events_changed), (1, 0));
    }

    #[test]
    fn a_rule_edit_is_named_in_rules_changed() {
        let mut room = Room::default();
        room.write(None, |txn| {
            let catalogue = NewCatalogue {
                uuid: "smart".to_owned(),
                name: "smart".to_owned(),
                ..NewCatalogue::default()
            };
            model::create_catalogue(txn, &catalogue).unwrap();
            model::create_catalogue(
                txn,
                &NewCatalogue {
                    uuid: "plain".to_owned(),
                    ..catalogue.clone()
                },
            )
            .unwrap();
        });
        room.write(Some((user(1), 0)), |txn| {
            let rule = json!({"all": true});
            assert!(model::set_catalogue_rule(txn, "smart", Some(&rule)).unwrap());
            model::create_event(txn, &event("a")).unwrap();
            assert!(model::add_catalogue_events(txn, "plain", &["a".to_owned()]));
        });

        let version = &room.versions()[0];

        assert_eq!(version.rules_changed, ["smart"]);
        assert_eq!(version.catalogues, ["plain", "smart"]);
    }

    #[test]
    fn an_import_origin_names_the_version_and_its_events_carry_via() {
        let tscat = Origin {
            id: "o1".to_owned(),
            kind: OriginKind::Import,
            name: "tscat".to_owned(),
            version: None,
            author: None,
            created: None,
            used: Vec::new(),
            parameters: None,
            derived_from: None,
        };
        let with_origin = |uuid: &str| EventRecord {
            origin: Some("o1".to_owned()),
            ..event(uuid)
        };
        let mut room = Room::default();
        room.write(Some((user(1), 0)), |txn| {
            model::create_origin(txn, &tscat).unwrap();
            model::create_event(txn, &with_origin("a")).unwrap();
        });
        room.write(Some((user(2), 1)), |txn| {
            model::create_event(txn, &with_origin("b")).unwrap();
        });

        let versions = room.versions();

        assert_eq!(versions[1].auto_name.as_deref(), Some("import: tscat"));
        assert_eq!(versions[1].via.as_deref(), Some("tscat"));
        assert_eq!(versions[0].auto_name, None);
        assert_eq!(versions[0].via.as_deref(), Some("tscat"));
    }

    #[test]
    fn versions_are_newest_first() {
        let mut room = Room::default();
        room.write(None, create("a"));
        room.write(Some((user(1), 0)), create("b"));
        room.write(Some((user(2), 0)), create("c"));

        assert_eq!(ranges(&room.versions()), [(2, 2), (1, 1), (0, 0)]);
    }

    #[test]
    fn a_record_that_does_not_decode_touches_nothing() {
        let touched = touched_by_records(&[Bytes::from_static(b"not an update")]);

        assert_eq!(touched, [Touched::default()]);
    }
}
