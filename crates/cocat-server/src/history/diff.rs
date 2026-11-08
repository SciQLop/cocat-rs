//! What a version changed: the model's reads of two states of a room, compared.

use std::collections::BTreeSet;

use bytes::Bytes;
use cocat_core::model::{self, CATALOGUES, CatalogueSummary, EVENTS, EventRecord, Status};
use serde::Serialize;
use serde_json::Value;
use yrs::{Doc, Map, Out, ReadTxn, Transact};

use super::versions::apply;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Change {
    Added,
    Changed,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct EventChange {
    pub uuid: String,
    pub change: Change,
    pub fields: Vec<String>,
    pub before: Option<EventRecord>,
    pub after: Option<EventRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct CatalogueChange {
    pub uuid: String,
    pub name: String,
    pub change: Change,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, utoipa::ToSchema)]
pub struct Diff {
    pub events: Vec<EventChange>,
    pub catalogues: Vec<CatalogueChange>,
}

/// The room after records `0..=end`; `u64::MAX` replays them all.
pub fn replay_until(records: &[Bytes], end: u64) -> Doc {
    let count = usize::try_from(end)
        .map_or(records.len(), |end| end.saturating_add(1))
        .min(records.len());
    let doc = Doc::new();
    for record in &records[..count] {
        apply(&doc, record);
    }
    doc
}

/// With a catalogue, only that catalogue and the events that are its members on either side.
pub fn diff(before: &Doc, after: &Doc, catalogue: Option<&str>) -> Diff {
    let (before, after) = (before.transact(), after.transact());
    let (events, catalogues) = match catalogue {
        Some(uuid) => (
            &members(&before, uuid) | &members(&after, uuid),
            BTreeSet::from([uuid.to_owned()]),
        ),
        None => (
            &keys(&before, EVENTS) | &keys(&after, EVENTS),
            &keys(&before, CATALOGUES) | &keys(&after, CATALOGUES),
        ),
    };
    Diff {
        events: events
            .into_iter()
            .filter_map(|uuid| {
                let old = model::event(&before, &uuid);
                let new = model::event(&after, &uuid);
                event_change(uuid, old, new)
            })
            .collect(),
        catalogues: catalogues
            .into_iter()
            .filter_map(|uuid| {
                let old = catalogue_state(&before, &uuid);
                let new = catalogue_state(&after, &uuid);
                catalogue_change(uuid, old, new)
            })
            .collect(),
    }
}

fn event_change(
    uuid: String,
    before: Option<EventRecord>,
    after: Option<EventRecord>,
) -> Option<EventChange> {
    let fields = match (&before, &after) {
        (Some(old), Some(new)) => changed_fields(fields_of(old), fields_of(new)),
        _ => Vec::new(),
    };
    let trashed = |record: &EventRecord| record.status == Status::Deleted;
    let change = change(
        before.as_ref().map(trashed),
        after.as_ref().map(trashed),
        &fields,
    )?;
    Some(EventChange {
        uuid,
        change,
        fields,
        before,
        after,
    })
}

/// A catalogue's fields as compared, and the events it lists.
struct CatalogueState {
    summary: CatalogueSummary,
    listed: BTreeSet<String>,
}

fn catalogue_state<T: ReadTxn>(txn: &T, uuid: &str) -> Option<CatalogueState> {
    Some(CatalogueState {
        summary: model::catalogue(txn, uuid)?,
        listed: listed(txn, uuid),
    })
}

fn catalogue_change(
    uuid: String,
    before: Option<CatalogueState>,
    after: Option<CatalogueState>,
) -> Option<CatalogueChange> {
    let mut fields = Vec::new();
    if let (Some(old), Some(new)) = (&before, &after) {
        fields = changed_fields(
            catalogue_fields(&old.summary),
            catalogue_fields(&new.summary),
        );
        if old.listed != new.listed {
            fields.push("events".to_owned());
            fields.sort();
        }
    }
    let trashed = |state: &CatalogueState| state.summary.deleted;
    let change = change(
        before.as_ref().map(trashed),
        after.as_ref().map(trashed),
        &fields,
    )?;
    let name = after.or(before)?.summary.name;
    Some(CatalogueChange {
        uuid,
        name,
        change,
        fields,
    })
}

/// From whether each side holds the object, and whether it is in the trash there.
fn change(before: Option<bool>, after: Option<bool>, fields: &[String]) -> Option<Change> {
    match (before, after) {
        (None, None) => None,
        (None, Some(_)) => Some(Change::Added),
        (Some(_), None) | (Some(false), Some(true)) => Some(Change::Removed),
        (Some(_), Some(_)) if !fields.is_empty() => Some(Change::Changed),
        (Some(_), Some(_)) => None,
    }
}

fn fields_of(value: &impl Serialize) -> serde_json::Map<String, Value> {
    match serde_json::to_value(value) {
        Ok(Value::Object(fields)) => fields,
        _ => serde_json::Map::new(),
    }
}

/// `event_count` counts live members, so it moves when a member is trashed: the
/// catalogue's own `events` set is compared instead.
fn catalogue_fields(summary: &CatalogueSummary) -> serde_json::Map<String, Value> {
    let mut fields = fields_of(summary);
    fields.remove("event_count");
    fields
}

/// The names whose values differ, sorted.
fn changed_fields(
    before: serde_json::Map<String, Value>,
    after: serde_json::Map<String, Value>,
) -> Vec<String> {
    let names: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    names
        .into_iter()
        .filter(|name| before.get(*name) != after.get(*name))
        .cloned()
        .collect()
}

/// What the catalogue lists plus what its rule yields, trashed members included.
fn members<T: ReadTxn>(txn: &T, catalogue: &str) -> BTreeSet<String> {
    let mut members = listed(txn, catalogue);
    if let Some(Ok(events)) = model::catalogue_events(txn, catalogue) {
        members.extend(events.into_iter().map(|event| event.uuid));
    }
    members
}

/// The events a catalogue lists, trashed ones included.
pub(crate) fn listed<T: ReadTxn>(txn: &T, catalogue: &str) -> BTreeSet<String> {
    let listed = txn
        .get_map(CATALOGUES)
        .and_then(|catalogues| catalogues.get(txn, catalogue));
    let Some(Out::YMap(catalogue)) = listed else {
        return BTreeSet::new();
    };
    match catalogue.get(txn, "events") {
        Some(Out::YMap(events)) => events.keys(txn).map(str::to_owned).collect(),
        _ => BTreeSet::new(),
    }
}

fn keys<T: ReadTxn>(txn: &T, root: &str) -> BTreeSet<String> {
    txn.get_map(root)
        .map(|map| map.keys(txn).map(str::to_owned).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use cocat_core::model::{self, EventField, NewCatalogue, Status};
    use cocat_core::time::Timestamp;
    use serde_json::json;
    use yrs::updates::decoder::Decode;
    use yrs::{ReadTxn, StateVector, Transact, TransactionMut, Update};

    use super::*;

    fn event(uuid: &str) -> EventRecord {
        let at = |nanos| Timestamp::from_unix_nanos(nanos).unwrap();
        EventRecord::new(uuid, at(0), at(1_000))
    }

    fn write<T>(doc: &Doc, edit: impl FnOnce(&mut TransactionMut) -> T) {
        edit(&mut doc.transact_mut());
    }

    fn copy(doc: &Doc) -> Doc {
        let update = doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        let copy = Doc::new();
        copy.transact_mut()
            .apply_update(Update::decode_v1(&update).unwrap())
            .unwrap();
        copy
    }

    fn catalogue(uuid: &str) -> NewCatalogue {
        NewCatalogue {
            uuid: uuid.to_owned(),
            name: uuid.to_owned(),
            ..NewCatalogue::default()
        }
    }

    fn changes(diff: &Diff) -> Vec<(&str, Change)> {
        diff.events
            .iter()
            .map(|event| (event.uuid.as_str(), event.change))
            .collect()
    }

    #[test]
    fn an_added_event_has_no_before() {
        let doc = Doc::new();
        let before = copy(&doc);
        write(&doc, |txn| model::create_event(txn, &event("a")).unwrap());

        let diff = diff(&before, &doc, None);

        assert_eq!(changes(&diff), [("a", Change::Added)]);
        assert_eq!(diff.events[0].before, None);
        assert_eq!(diff.events[0].after, Some(event("a")));
    }

    #[test]
    fn a_changed_event_lists_only_its_changed_fields() {
        let doc = Doc::new();
        write(&doc, |txn| model::create_event(txn, &event("a")).unwrap());
        let before = copy(&doc);
        write(&doc, |txn| {
            model::set_event_field(txn, "a", &EventField::Confidence(Some(0.5))).unwrap();
            model::set_event_field(txn, "a", &EventField::Status(Status::Reviewed)).unwrap();
        });

        let diff = diff(&before, &doc, None);

        assert_eq!(changes(&diff), [("a", Change::Changed)]);
        assert_eq!(diff.events[0].fields, ["confidence", "status"]);
        assert_eq!(diff.events[0].before, Some(event("a")));
    }

    #[test]
    fn an_unchanged_room_has_an_empty_diff() {
        let doc = Doc::new();
        write(&doc, |txn| {
            model::create_event(txn, &event("a")).unwrap();
            model::create_catalogue(txn, &catalogue("c")).unwrap();
        });

        assert_eq!(diff(&copy(&doc), &doc, None), Diff::default());
    }

    #[test]
    fn a_trashed_event_is_removed() {
        let doc = Doc::new();
        write(&doc, |txn| {
            model::create_event(txn, &event("trashed")).unwrap();
            model::create_event(txn, &event("purged")).unwrap();
        });
        let before = copy(&doc);
        write(&doc, |txn| {
            assert!(model::delete_event(txn, "trashed"));
            assert!(model::purge_event(txn, "purged"));
        });

        let diff = diff(&before, &doc, None);

        assert_eq!(
            changes(&diff),
            [("purged", Change::Removed), ("trashed", Change::Removed)]
        );
        assert_eq!(diff.events[0].after, None);
        let trashed = diff.events[1].after.as_ref().unwrap();
        assert_eq!(trashed.status, Status::Deleted);
        assert_eq!(diff.events[1].fields, ["status"]);
    }

    #[test]
    fn a_catalogue_filter_keeps_only_its_members() {
        let doc = Doc::new();
        write(&doc, |txn| {
            model::create_event(txn, &event("in")).unwrap();
            model::create_event(txn, &event("out")).unwrap();
            model::create_catalogue(txn, &catalogue("c")).unwrap();
            model::create_catalogue(txn, &catalogue("other")).unwrap();
            assert!(model::add_catalogue_events(txn, "c", &["in".to_owned()]));
        });
        let before = copy(&doc);
        write(&doc, |txn| {
            for uuid in ["in", "out"] {
                model::set_event_field(txn, uuid, &EventField::Confidence(Some(0.5))).unwrap();
            }
            assert!(model::rename_catalogue(txn, "c", "renamed"));
            assert!(model::rename_catalogue(txn, "other", "renamed too"));
        });

        let diff = diff(&before, &doc, Some("c"));

        assert_eq!(changes(&diff), [("in", Change::Changed)]);
        assert_eq!(diff.catalogues.len(), 1);
        assert_eq!(diff.catalogues[0].uuid, "c");
        assert_eq!(diff.catalogues[0].name, "renamed");
        assert_eq!(diff.catalogues[0].fields, ["name"]);
    }

    #[test]
    fn an_event_trashed_out_of_a_catalogue_stays_in_its_filter() {
        let doc = Doc::new();
        write(&doc, |txn| {
            model::create_event(txn, &event("in")).unwrap();
            model::create_catalogue(txn, &catalogue("c")).unwrap();
            assert!(model::add_catalogue_events(txn, "c", &["in".to_owned()]));
        });
        let before = copy(&doc);
        write(&doc, |txn| assert!(model::delete_event(txn, "in")));

        let diff = diff(&before, &doc, Some("c"));

        assert_eq!(changes(&diff), [("in", Change::Removed)]);
    }

    #[test]
    fn a_smart_catalogue_filter_keeps_the_events_its_rule_yields() {
        let doc = Doc::new();
        write(&doc, |txn| {
            model::create_event(txn, &event("a")).unwrap();
            model::create_catalogue(txn, &catalogue("smart")).unwrap();
            let rule = json!({"all": true});
            assert!(model::set_catalogue_rule(txn, "smart", Some(&rule)).unwrap());
        });
        let before = copy(&doc);
        write(&doc, |txn| {
            model::set_event_field(txn, "a", &EventField::Confidence(Some(0.5))).unwrap();
            model::create_event(txn, &event("b")).unwrap();
        });

        let diff = diff(&before, &doc, Some("smart"));

        assert_eq!(
            changes(&diff),
            [("a", Change::Changed), ("b", Change::Added)]
        );
        assert_eq!(diff.catalogues, []);
    }

    #[test]
    fn a_member_added_to_a_catalogue_shows_events_in_its_fields() {
        let doc = Doc::new();
        write(&doc, |txn| {
            model::create_event(txn, &event("a")).unwrap();
            model::create_catalogue(txn, &catalogue("c")).unwrap();
        });
        let before = copy(&doc);
        write(&doc, |txn| {
            assert!(model::add_catalogue_events(txn, "c", &["a".to_owned()]))
        });

        let diff = diff(&before, &doc, None);

        assert_eq!(
            diff.catalogues,
            [CatalogueChange {
                uuid: "c".to_owned(),
                name: "c".to_owned(),
                change: Change::Changed,
                fields: vec!["events".to_owned()],
            }]
        );
        assert_eq!(diff.events, []);
    }

    #[test]
    fn a_trashed_catalogue_is_removed_and_a_new_one_added() {
        let doc = Doc::new();
        write(&doc, |txn| {
            model::create_catalogue(txn, &catalogue("old")).unwrap()
        });
        let before = copy(&doc);
        write(&doc, |txn| {
            assert!(model::delete_catalogue(txn, "old"));
            model::create_catalogue(txn, &catalogue("new")).unwrap();
        });

        let kinds: Vec<_> = diff(&before, &doc, None)
            .catalogues
            .into_iter()
            .map(|catalogue| (catalogue.uuid, catalogue.change))
            .collect();

        assert_eq!(
            kinds,
            [
                ("new".to_owned(), Change::Added),
                ("old".to_owned(), Change::Removed)
            ]
        );
    }

    #[test]
    fn replaying_until_a_record_stops_there() {
        let doc = Doc::new();
        let records: Vec<Bytes> = ["a", "b"]
            .into_iter()
            .map(|uuid| {
                let before = doc.transact().state_vector();
                let mut txn = doc.transact_mut();
                model::create_event(&mut txn, &event(uuid)).unwrap();
                Bytes::from(txn.encode_diff_v1(&before))
            })
            .collect();

        let first = replay_until(&records, 0);
        let all = replay_until(&records, u64::MAX);

        assert!(model::event(&first.transact(), "a").is_some());
        assert!(model::event(&first.transact(), "b").is_none());
        assert!(model::event(&all.transact(), "b").is_some());
    }
}
