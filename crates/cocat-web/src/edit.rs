//! The page's writes. Each takes the caller's transaction, so one edit is one transaction,
//! and checks everything it was given before it writes anything: a refused edit leaves the
//! room as it was.

use cocat_core::model::{self, EventField, EventRecord, NewCatalogue, Status, WriteError};
use cocat_core::rules::{self, RuleError};
use cocat_core::time::Timestamp;
use serde_json::{Map, Value};
use thiserror::Error;
use yrs::TransactionMut;

use crate::format::time_text;
use crate::rule_text::describe;

const MINUTE: i64 = 60 * 1_000_000_000;

#[derive(Debug, Error)]
pub enum EditError {
    #[error("this event is no longer in the project")]
    NoEvent,
    #[error("this catalogue is no longer in the project")]
    NoCatalogue,
    #[error("{0} is not a status an event can be given here")]
    Status(String),
    #[error("{0:?} is not a number")]
    Number(String),
    #[error("{0:?} is not a time: write it as 2019-11-03 14:02:11")]
    Time(String),
    #[error("the stop is before the start")]
    StopBeforeStart,
    #[error("an attribute needs a name")]
    NoKey,
    #[error("a catalogue needs a name")]
    NoName,
    #[error("the rule is not JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Rule(#[from] RuleError),
    #[error(transparent)]
    Write(#[from] WriteError),
}

/// The statuses a person picks from: `deleted` is the trash's, reached by trashing.
pub const STATUSES: [Status; 5] = [
    Status::Preliminary,
    Status::Confirmed,
    Status::Reviewed,
    Status::Final,
    Status::Rejected,
];

pub fn set_status(txn: &mut TransactionMut<'_>, uuid: &str, name: &str) -> Result<(), EditError> {
    let status = Status::from_name(name)
        .filter(|status| STATUSES.contains(status))
        .ok_or_else(|| EditError::Status(name.to_owned()))?;
    set_field(txn, uuid, &EventField::Status(status))
}

/// An empty text removes the confidence.
pub fn set_confidence(
    txn: &mut TransactionMut<'_>,
    uuid: &str,
    text: &str,
) -> Result<(), EditError> {
    let text = text.trim();
    let confidence = match text {
        "" => None,
        _ => Some(
            text.parse::<f64>()
                .map_err(|_| EditError::Number(text.to_owned()))?,
        ),
    };
    set_field(txn, uuid, &EventField::Confidence(confidence))
}

/// Sets the range from the texts the panel shows. A text left as shown keeps the exact time
/// it shows to the second only.
pub fn set_range(
    txn: &mut TransactionMut<'_>,
    uuid: &str,
    start: &str,
    stop: &str,
) -> Result<(), EditError> {
    let record = existing(txn, uuid)?;
    let (start, stop) = (time_or(start, record.start)?, time_or(stop, record.stop)?);
    if stop < start {
        return Err(EditError::StopBeforeStart);
    }
    found(
        model::set_event_range(txn, uuid, start, stop)?,
        EditError::NoEvent,
    )
}

/// Adds the tags of a comma-separated list the event does not carry yet.
pub fn add_tags(txn: &mut TransactionMut<'_>, uuid: &str, list: &str) -> Result<(), EditError> {
    let mut tags = existing(txn, uuid)?.tags;
    tags.extend(
        list.split(',')
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(str::to_owned),
    );
    set_field(txn, uuid, &EventField::Tags(tags))
}

pub fn remove_tag(txn: &mut TransactionMut<'_>, uuid: &str, tag: &str) -> Result<(), EditError> {
    let mut tags = existing(txn, uuid)?.tags;
    tags.retain(|kept| kept != tag);
    set_field(txn, uuid, &EventField::Tags(tags))
}

/// A blank text removes the comment.
pub fn set_comment(txn: &mut TransactionMut<'_>, uuid: &str, text: &str) -> Result<(), EditError> {
    let text = text.trim();
    found(
        model::set_comment(txn, uuid, (!text.is_empty()).then_some(text)),
        EditError::NoEvent,
    )
}

/// What a typed attribute value means: JSON when it reads as JSON (`2.4`, `true`,
/// `"2.4"`), else the text itself.
pub fn attribute_value(text: &str) -> Value {
    let text = text.trim();
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned()))
}

pub fn set_attribute(
    txn: &mut TransactionMut<'_>,
    uuid: &str,
    key: &str,
    value: &str,
) -> Result<(), EditError> {
    let key = key.trim();
    if key.is_empty() {
        return Err(EditError::NoKey);
    }
    let attributes = Map::from_iter([(key.to_owned(), attribute_value(value))]);
    model::set_event_attributes(txn, uuid, &attributes)?
        .map(drop)
        .ok_or(EditError::NoEvent)
}

pub fn remove_attribute(
    txn: &mut TransactionMut<'_>,
    uuid: &str,
    key: &str,
) -> Result<(), EditError> {
    found(
        model::remove_event_attributes(txn, uuid, &[key.to_owned()]),
        EditError::NoEvent,
    )
}

pub fn trash_event(txn: &mut TransactionMut<'_>, uuid: &str) -> Result<(), EditError> {
    found(model::delete_event(txn, uuid), EditError::NoEvent)
}

pub fn restore_event(txn: &mut TransactionMut<'_>, uuid: &str) -> Result<(), EditError> {
    found(model::restore_event(txn, uuid), EditError::NoEvent)
}

pub fn purge_event(txn: &mut TransactionMut<'_>, uuid: &str) -> Result<(), EditError> {
    found(model::purge_event(txn, uuid), EditError::NoEvent)
}

pub fn list_event(
    txn: &mut TransactionMut<'_>,
    catalogue: &str,
    event: &str,
) -> Result<(), EditError> {
    found(
        model::add_catalogue_events(txn, catalogue, &[event.to_owned()]),
        EditError::NoCatalogue,
    )
}

pub fn unlist_event(
    txn: &mut TransactionMut<'_>,
    catalogue: &str,
    event: &str,
) -> Result<(), EditError> {
    found(
        model::remove_catalogue_events(txn, catalogue, &[event.to_owned()]),
        EditError::NoCatalogue,
    )
}

/// A new event from a minute before `now` to a minute after, listed in `catalogue` when
/// one is given. Answers its uuid.
pub fn new_event(
    txn: &mut TransactionMut<'_>,
    catalogue: Option<&str>,
    now: Timestamp,
    author: &str,
) -> Result<String, EditError> {
    let shifted = |by: i64| {
        now.unix_nanos()
            .and_then(|nanos| Timestamp::from_unix_nanos(nanos + by))
            .ok_or(WriteError::Time(now))
    };
    let mut record = EventRecord::new(new_uuid(), shifted(-MINUTE)?, shifted(MINUTE)?);
    record.author = Some(author.to_owned());
    model::create_event(txn, &record)?;
    if let Some(catalogue) = catalogue {
        model::add_catalogue_events(txn, catalogue, &[record.uuid.clone()]);
    }
    Ok(record.uuid)
}

/// Answers the new catalogue's uuid.
pub fn new_catalogue(
    txn: &mut TransactionMut<'_>,
    name: &str,
    author: &str,
) -> Result<String, EditError> {
    let catalogue = named(name, author)?;
    model::create_catalogue(txn, &catalogue)?;
    Ok(catalogue.uuid)
}

/// The rule a text holds, when it is JSON and a rule.
pub fn check_rule(text: &str) -> Result<Value, EditError> {
    let rule: Value = serde_json::from_str(text)?;
    rules::parse(&rule)?;
    Ok(rule)
}

/// What the rule editor says of the text typed so far: the rule in words, or why it is not
/// one. `name` gives a catalogue's name from its uuid.
pub fn rule_preview(
    text: &str,
    name: &dyn Fn(&str) -> Option<String>,
) -> Result<String, EditError> {
    Ok(describe(&check_rule(text)?, name)?)
}

pub fn set_rule(txn: &mut TransactionMut<'_>, uuid: &str, text: &str) -> Result<(), EditError> {
    let rule = check_rule(text)?;
    found(
        model::set_catalogue_rule(txn, uuid, Some(&rule))?,
        EditError::NoCatalogue,
    )
}

/// A static catalogue named `name` listing what catalogue `uuid` holds now. Answers its uuid.
pub fn materialize(
    txn: &mut TransactionMut<'_>,
    uuid: &str,
    name: &str,
    author: &str,
) -> Result<String, EditError> {
    let catalogue = named(name, author)?;
    found(
        model::materialize(txn, uuid, &catalogue)?,
        EditError::NoCatalogue,
    )?;
    Ok(catalogue.uuid)
}

pub fn trash_catalogue(txn: &mut TransactionMut<'_>, uuid: &str) -> Result<(), EditError> {
    found(model::delete_catalogue(txn, uuid), EditError::NoCatalogue)
}

pub fn restore_catalogue(txn: &mut TransactionMut<'_>, uuid: &str) -> Result<(), EditError> {
    found(model::restore_catalogue(txn, uuid), EditError::NoCatalogue)
}

pub fn purge_catalogue(txn: &mut TransactionMut<'_>, uuid: &str) -> Result<(), EditError> {
    found(model::purge_catalogue(txn, uuid), EditError::NoCatalogue)
}

fn set_field(
    txn: &mut TransactionMut<'_>,
    uuid: &str,
    field: &EventField,
) -> Result<(), EditError> {
    found(
        model::set_event_field(txn, uuid, field)?,
        EditError::NoEvent,
    )
}

fn existing(txn: &TransactionMut<'_>, uuid: &str) -> Result<EventRecord, EditError> {
    model::event(txn, uuid).ok_or(EditError::NoEvent)
}

fn found(written: bool, missing: EditError) -> Result<(), EditError> {
    if written { Ok(()) } else { Err(missing) }
}

fn time_or(text: &str, current: Timestamp) -> Result<Timestamp, EditError> {
    if current.unix_nanos().map(time_text).as_deref() == Some(text.trim()) {
        return Ok(current);
    }
    Timestamp::parse(text).ok_or_else(|| EditError::Time(text.trim().to_owned()))
}

fn named(name: &str, author: &str) -> Result<NewCatalogue, EditError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(EditError::NoName);
    }
    Ok(NewCatalogue {
        uuid: new_uuid(),
        name: name.to_owned(),
        author: Some(author.to_owned()),
        ..NewCatalogue::default()
    })
}

fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use cocat_core::model::{self, Status};
    use serde_json::json;
    use yrs::updates::decoder::Decode;
    use yrs::{Doc, ReadTxn, StateVector, Transact, Update};

    use super::*;
    use crate::fixture::{DAY0, SECOND, at, room};

    fn edited<R>(doc: &Doc, edit: impl FnOnce(&mut TransactionMut<'_>) -> R) -> R {
        edit(&mut doc.transact_mut())
    }

    fn event(doc: &Doc, uuid: &str) -> model::EventRecord {
        model::event(&doc.transact(), uuid).unwrap()
    }

    /// An edit refused by its validation leaves the room as it was.
    fn refused(
        doc: &Doc,
        edit: impl FnOnce(&mut TransactionMut<'_>) -> Result<(), EditError>,
    ) -> String {
        let before = doc.transact().state_vector();
        let error = edited(doc, edit).unwrap_err().to_string();
        assert_eq!(doc.transact().state_vector(), before, "{error}");
        error
    }

    #[test]
    fn the_status_is_set_by_its_name() {
        let doc = room();
        edited(&doc, |txn| set_status(txn, "e1", "reviewed")).unwrap();
        assert_eq!(event(&doc, "e1").status, Status::Reviewed);
    }

    #[test]
    fn an_unknown_status_writes_nothing() {
        let doc = room();
        let error = refused(&doc, |txn| set_status(txn, "e1", "deleted"));
        assert_eq!(error, "deleted is not a status an event can be given here");
    }

    #[test]
    fn the_confidence_is_a_number_between_0_and_1_or_nothing() {
        let doc = room();
        edited(&doc, |txn| set_confidence(txn, "e1", " 0.9 ")).unwrap();
        assert_eq!(event(&doc, "e1").confidence, Some(0.9));
        edited(&doc, |txn| set_confidence(txn, "e1", "")).unwrap();
        assert_eq!(event(&doc, "e1").confidence, None);
    }

    #[test]
    fn a_confidence_outside_0_and_1_writes_nothing() {
        let doc = room();
        let error = refused(&doc, |txn| set_confidence(txn, "e1", "1.5"));
        assert_eq!(error, "confidence 1.5 is not between 0 and 1");
        let error = refused(&doc, |txn| set_confidence(txn, "e1", "high"));
        assert_eq!(error, "\"high\" is not a number");
    }

    #[test]
    fn the_range_is_set_from_its_texts() {
        let doc = room();
        edited(&doc, |txn| {
            set_range(txn, "e1", "2019-11-03 14:00:00", "2019-11-03 14:05:00")
        })
        .unwrap();
        let record = event(&doc, "e1");
        assert_eq!(record.start, at(DAY0 + 14 * 3600 * SECOND));
        assert_eq!(record.stop, at(DAY0 + (14 * 3600 + 300) * SECOND));
    }

    #[test]
    fn a_range_text_left_as_shown_keeps_its_exact_time() {
        let doc = room();
        let precise = at(DAY0 + 14 * 3600 * SECOND + 250_000_000);
        edited(&doc, |txn| {
            model::set_event_range(txn, "e1", precise, at(DAY0 + 15 * 3600 * SECOND))
        })
        .unwrap();
        edited(&doc, |txn| {
            set_range(txn, "e1", "2019-11-03 14:00:00", "2019-11-03 16:00:00")
        })
        .unwrap();
        assert_eq!(event(&doc, "e1").start, precise);
    }

    #[test]
    fn a_stop_before_its_start_writes_nothing() {
        let doc = room();
        let error = refused(&doc, |txn| {
            set_range(txn, "e1", "2019-11-03 14:00:00", "2019-11-03 13:00:00")
        });
        assert_eq!(error, "the stop is before the start");
    }

    #[test]
    fn a_time_that_does_not_read_writes_nothing() {
        let doc = room();
        let error = refused(&doc, |txn| {
            set_range(txn, "e1", "yesterday", "2019-11-03 13:00:00")
        });
        assert_eq!(
            error,
            "\"yesterday\" is not a time: write it as 2019-11-03 14:02:11"
        );
    }

    #[test]
    fn tags_are_added_from_a_comma_list_once_each_and_removed_one_by_one() {
        let doc = room();
        edited(&doc, |txn| add_tags(txn, "e1", "burst, sw ,, kaw")).unwrap();
        assert_eq!(event(&doc, "e1").tags, ["burst", "kaw", "sheath", "sw"]);
        edited(&doc, |txn| remove_tag(txn, "e1", "sheath")).unwrap();
        assert_eq!(event(&doc, "e1").tags, ["burst", "kaw", "sw"]);
    }

    #[test]
    fn the_comment_is_set_and_an_empty_one_removed() {
        let doc = room();
        edited(&doc, |txn| set_comment(txn, "e1", "Clear crossing")).unwrap();
        assert_eq!(
            model::comment(&doc.transact(), "e1").as_deref(),
            Some("Clear crossing")
        );
        edited(&doc, |txn| set_comment(txn, "e1", "  ")).unwrap();
        assert_eq!(model::comment(&doc.transact(), "e1"), None);
    }

    #[test]
    fn attribute_values_read_as_json_else_as_text() {
        assert_eq!(attribute_value("2.4"), json!(2.4));
        assert_eq!(attribute_value("true"), json!(true));
        assert_eq!(attribute_value("\"2.4\""), json!("2.4"));
        assert_eq!(attribute_value(" MMS1 "), json!("MMS1"));
    }

    #[test]
    fn attributes_are_added_changed_and_removed() {
        let doc = room();
        edited(&doc, |txn| {
            set_attribute(txn, "e1", " region ", "foreshock")
        })
        .unwrap();
        edited(&doc, |txn| set_attribute(txn, "e1", "amplitude", "3.1")).unwrap();
        edited(&doc, |txn| remove_attribute(txn, "e1", "spacecraft")).unwrap();
        assert_eq!(
            json!(event(&doc, "e1").attributes),
            json!({"region": "foreshock", "amplitude": 3.1})
        );
    }

    #[test]
    fn an_attribute_without_a_name_writes_nothing() {
        let doc = room();
        let error = refused(&doc, |txn| set_attribute(txn, "e1", " ", "1"));
        assert_eq!(error, "an attribute needs a name");
    }

    #[test]
    fn an_edit_of_an_event_the_room_lost_says_so() {
        let doc = room();
        let error = refused(&doc, |txn| set_status(txn, "gone", "final"));
        assert_eq!(error, "this event is no longer in the project");
    }

    #[test]
    fn an_event_goes_to_the_trash_comes_back_and_is_purged() {
        let doc = room();
        edited(&doc, |txn| trash_event(txn, "e1")).unwrap();
        assert_eq!(event(&doc, "e1").status, Status::Deleted);
        edited(&doc, |txn| restore_event(txn, "e1")).unwrap();
        assert_eq!(event(&doc, "e1").status, Status::Confirmed);
        edited(&doc, |txn| purge_event(txn, "e1")).unwrap();
        assert!(model::event(&doc.transact(), "e1").is_none());
    }

    #[test]
    fn an_event_is_listed_in_and_taken_out_of_a_catalogue() {
        let doc = room();
        edited(&doc, |txn| list_event(txn, "c-static", "e2")).unwrap();
        assert_eq!(model::event_catalogues(&doc.transact(), "e2"), ["c-static"]);
        edited(&doc, |txn| unlist_event(txn, "c-static", "e2")).unwrap();
        assert!(model::event_catalogues(&doc.transact(), "e2").is_empty());
    }

    #[test]
    fn a_new_event_spans_a_minute_each_side_of_now_in_the_catalogue() {
        let doc = room();
        let now = at(DAY0 + 10 * SECOND * 3600);
        let uuid = edited(&doc, |txn| {
            new_event(txn, Some("c-static"), now, "ann@example.org")
        })
        .unwrap();
        let record = event(&doc, &uuid);
        assert_eq!(record.start, at(DAY0 + (10 * 3600 - 60) * SECOND));
        assert_eq!(record.stop, at(DAY0 + (10 * 3600 + 60) * SECOND));
        assert_eq!(record.author.as_deref(), Some("ann@example.org"));
        assert_eq!(
            model::event_catalogues(&doc.transact(), &uuid),
            ["c-static"]
        );
    }

    #[test]
    fn a_new_catalogue_needs_a_name() {
        let doc = room();
        let uuid = edited(&doc, |txn| {
            new_catalogue(txn, " Quiet times ", "ann@example.org")
        })
        .unwrap();
        let catalogue = model::catalogue(&doc.transact(), &uuid).unwrap();
        assert_eq!(catalogue.name, "Quiet times");
        assert_eq!(catalogue.author.as_deref(), Some("ann@example.org"));
        let error = refused(&doc, |txn| {
            new_catalogue(txn, "  ", "ann@example.org").map(drop)
        });
        assert_eq!(error, "a catalogue needs a name");
    }

    #[test]
    fn a_rule_is_checked_before_it_is_saved() {
        let rule =
            check_rule(r#"{"filter": {"all": true}, "where": {"contains": ["tags", "sw"]}}"#);
        assert_eq!(
            rule.unwrap(),
            json!({"filter": {"all": true}, "where": {"contains": ["tags", "sw"]}})
        );
        assert_eq!(
            check_rule(r#"{"filter": {"all": true}"#)
                .unwrap_err()
                .to_string(),
            "the rule is not JSON: EOF while parsing an object at line 1 column 24"
        );
        assert_eq!(
            check_rule(r#"{"filter": {"all": false}}"#)
                .unwrap_err()
                .to_string(),
            "rule.filter.all: \"all\" must be true"
        );
    }

    #[test]
    fn a_rule_being_typed_reads_in_words_or_gives_its_error() {
        let name = |uuid: &str| (uuid == "c-static").then(|| "Bow shock crossings".to_owned());
        assert_eq!(
            rule_preview(r#"{"catalogue": "c-static"}"#, &name).unwrap(),
            "“Bow shock crossings”"
        );
        assert!(rule_preview("[", &name).is_err());
    }

    #[test]
    fn a_valid_rule_is_saved_and_an_invalid_one_writes_nothing() {
        let doc = room();
        edited(&doc, |txn| {
            set_rule(txn, "c-smart", r#"{"catalogue": "c-static"}"#)
        })
        .unwrap();
        let rule = model::catalogue(&doc.transact(), "c-smart").unwrap().rule;
        assert_eq!(rule, Some(json!({"catalogue": "c-static"})));
        refused(&doc, |txn| set_rule(txn, "c-smart", "{"));
    }

    #[test]
    fn materializing_makes_a_static_catalogue_of_the_members() {
        let doc = room();
        let uuid = edited(&doc, |txn| {
            materialize(txn, "c-smart", "Waves, frozen", "ann@example.org")
        })
        .unwrap();
        let catalogue = model::catalogue(&doc.transact(), &uuid).unwrap();
        assert_eq!(catalogue.name, "Waves, frozen");
        assert_eq!(catalogue.rule, None);
        assert_eq!(catalogue.event_count, 3);
    }

    #[test]
    fn a_catalogue_goes_to_the_trash_comes_back_and_is_purged() {
        let doc = room();
        edited(&doc, |txn| trash_catalogue(txn, "c-static")).unwrap();
        assert!(
            model::catalogue(&doc.transact(), "c-static")
                .unwrap()
                .deleted
        );
        edited(&doc, |txn| restore_catalogue(txn, "c-static")).unwrap();
        assert!(
            !model::catalogue(&doc.transact(), "c-static")
                .unwrap()
                .deleted
        );
        edited(&doc, |txn| purge_catalogue(txn, "c-static")).unwrap();
        assert!(model::catalogue(&doc.transact(), "c-static").is_none());
    }

    fn exchange(from: &Doc, to: &Doc) {
        let update = from
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        to.transact_mut()
            .apply_update(Update::decode_v1(&update).unwrap())
            .unwrap();
    }

    #[test]
    fn two_replicas_editing_one_confidence_at_once_converge() {
        let (left, right) = (room(), Doc::new());
        exchange(&left, &right);
        edited(&left, |txn| set_confidence(txn, "e1", "0.1")).unwrap();
        edited(&right, |txn| set_confidence(txn, "e1", "0.9")).unwrap();
        exchange(&left, &right);
        exchange(&right, &left);
        let (mine, theirs) = (
            event(&left, "e1").confidence,
            event(&right, "e1").confidence,
        );
        assert_eq!(mine, theirs);
        assert!(mine == Some(0.1) || mine == Some(0.9));
    }
}
