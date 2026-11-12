//! The sidebar's list: the room's catalogues with their member counts, and the trash.

use cocat_core::model;
use serde_json::{Map, Value};
use yrs::{Map as _, ReadTxn};

use crate::format::value_text;

/// The catalogue attribute its page shows under the title, and edits as text.
pub const DESCRIPTION: &str = "description";

/// How many attributes the About box lists while folded: three rows of two.
const FOLDED_ATTRIBUTES: usize = 6;

/// How many of `count` attributes the About box lists, and how many more its "+ N more"
/// button names while some stay hidden. Hiding a single one is not worth the button.
pub fn listed_attributes(count: usize, open: bool) -> (usize, Option<usize>) {
    if open || count <= FOLDED_ATTRIBUTES + 1 {
        return (count, None);
    }
    (FOLDED_ATTRIBUTES, Some(count - FOLDED_ATTRIBUTES))
}

/// A catalogue's description: its `description` attribute, when it holds text.
pub fn description(attributes: &Map<String, Value>) -> Option<String> {
    attributes
        .get(DESCRIPTION)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
}

/// Its other attributes as the editor lists them, name and value text.
pub fn attribute_rows(attributes: &Map<String, Value>) -> Vec<(String, String)> {
    attributes
        .iter()
        .filter(|(key, _)| key.as_str() != DESCRIPTION)
        .map(|(key, value)| (key.clone(), value_text(value)))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CatalogueItem {
    pub uuid: String,
    pub name: String,
    pub smart: bool,
    /// `None` when the rule does not evaluate.
    pub count: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Named {
    pub uuid: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogueList {
    pub catalogues: Vec<CatalogueItem>,
    /// What the trash holds: events and catalogues.
    pub trash: usize,
    pub trashed: Vec<Named>,
}

// simplify: every smart catalogue's rule runs on each room update (about 70 ms each at
// 100k events). Cache counts per catalogue keyed on the room's state vector if big rooms
// with many smart catalogues stutter.
pub fn catalogue_list<T: ReadTxn>(txn: &T) -> CatalogueList {
    let catalogues = model::catalogues(txn)
        .into_iter()
        .map(|summary| {
            let smart = summary.rule.is_some();
            let count = if smart {
                model::catalogue_members(txn, &summary.uuid)
                    .and_then(Result::ok)
                    .map(|members| members.len())
            } else {
                Some(summary.event_count)
            };
            CatalogueItem {
                uuid: summary.uuid,
                name: summary.name,
                smart,
                count,
            }
        })
        .collect();
    let trashed = trashed_catalogues(txn);
    CatalogueList {
        catalogues,
        trash: model::trash(txn).len() + trashed.len(),
        trashed,
    }
}

/// The catalogues in the trash, by name.
fn trashed_catalogues<T: ReadTxn>(txn: &T) -> Vec<Named> {
    let Some(root) = txn.get_map(model::CATALOGUES) else {
        return Vec::new();
    };
    let mut trashed: Vec<Named> = root
        .keys(txn)
        .filter_map(|uuid| model::catalogue(txn, uuid))
        .filter(|catalogue| catalogue.deleted)
        .map(|catalogue| Named {
            uuid: catalogue.uuid,
            name: catalogue.name,
        })
        .collect();
    trashed.sort_by(|left, right| left.name.cmp(&right.name));
    trashed
}

#[cfg(test)]
mod tests {
    use cocat_core::model::{self, EventRecord, NewCatalogue};
    use cocat_core::time::Timestamp;
    use serde_json::json;
    use yrs::{Doc, Transact};

    use super::*;

    fn at(second: i64) -> Timestamp {
        Timestamp::from_unix_nanos(1_573_000_000_000_000_000 + second * 1_000_000_000).unwrap()
    }

    fn event(uuid: &str, second: i64, tags: &[&str]) -> EventRecord {
        let mut record = EventRecord::new(uuid, at(second), at(second + 30));
        record.tags = tags.iter().map(|tag| (*tag).to_owned()).collect();
        record
    }

    fn catalogue(uuid: &str, name: &str) -> NewCatalogue {
        NewCatalogue {
            uuid: uuid.into(),
            name: name.into(),
            ..NewCatalogue::default()
        }
    }

    fn room() -> Doc {
        let doc = Doc::new();
        {
            let mut txn = doc.transact_mut();
            for record in [
                event("e1", 0, &["sw"]),
                event("e2", 100, &["sw", "burst"]),
                event("e3", 200, &["sheath"]),
                event("e4", 300, &["sw"]),
            ] {
                model::create_event(&mut txn, &record).unwrap();
            }
            model::delete_event(&mut txn, "e4");
            model::create_catalogue(&mut txn, &catalogue("c-b", "Bow shock crossings")).unwrap();
            model::add_catalogue_events(&mut txn, "c-b", &["e1".into(), "e3".into()]);
            model::create_catalogue(&mut txn, &catalogue("c-s", "Solitary waves")).unwrap();
            let rule = json!({"filter": {"all": true}, "where": {"contains": ["tags", "sw"]}});
            model::set_catalogue_rule(&mut txn, "c-s", Some(&rule)).unwrap();
            model::create_catalogue(&mut txn, &catalogue("c-old", "Old")).unwrap();
            model::delete_catalogue(&mut txn, "c-old");
        }
        doc
    }

    #[test]
    fn the_list_names_each_catalogue_with_its_members_and_marks_smart_ones() {
        let list = catalogue_list(&room().transact());
        assert_eq!(
            list.catalogues,
            [
                CatalogueItem {
                    uuid: "c-b".into(),
                    name: "Bow shock crossings".into(),
                    smart: false,
                    count: Some(2),
                },
                CatalogueItem {
                    uuid: "c-s".into(),
                    name: "Solitary waves".into(),
                    smart: true,
                    count: Some(2),
                },
            ]
        );
        assert_eq!(list.trash, 2);
        assert_eq!(
            list.trashed,
            [Named {
                uuid: "c-old".into(),
                name: "Old".into()
            }]
        );
    }

    #[test]
    fn a_smart_catalogue_whose_rule_fails_has_no_count() {
        let doc = room();
        let rule = json!({"catalogue": "c-s"});
        model::set_catalogue_rule(&mut doc.transact_mut(), "c-s", Some(&rule)).unwrap();
        let list = catalogue_list(&doc.transact());
        assert_eq!(list.catalogues[1].count, None);
    }

    #[test]
    fn an_empty_room_lists_nothing() {
        let list = catalogue_list(&Doc::new().transact());
        assert!(list.catalogues.is_empty());
        assert_eq!(list.trash, 0);
    }

    #[test]
    fn the_heading_folds_a_long_attribute_list_but_never_hides_just_one() {
        assert_eq!(listed_attributes(3, false), (3, None));
        assert_eq!(
            listed_attributes(7, false),
            (7, None),
            "one hidden is not worth a button"
        );
        assert_eq!(
            listed_attributes(20, false),
            (6, Some(14)),
            "three rows of two, and how many more"
        );
        assert_eq!(listed_attributes(20, true), (20, None));
    }
}
