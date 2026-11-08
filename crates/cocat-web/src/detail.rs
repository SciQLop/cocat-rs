//! What the event panel shows about one event.

use cocat_core::model::{self, OriginKind, Status};
use yrs::ReadTxn;

use crate::catalogues::Named;
use crate::format::{clock_text, date_text, duration_text, time_text, value_text};

#[derive(Debug, Clone, PartialEq)]
pub struct EventDetail {
    pub uuid: String,
    pub start: String,
    pub stop: String,
    /// The stop in full, as its input shows it.
    pub stop_time: String,
    pub error: Option<String>,
    pub duration: String,
    pub status: String,
    pub deleted: bool,
    pub confidence: Option<f64>,
    pub tags: Vec<String>,
    pub author: Option<String>,
    pub origin: Option<String>,
    pub comment: Option<String>,
    pub attributes: Vec<(String, String)>,
    /// The catalogues that list the event.
    pub catalogues: Vec<Named>,
    /// The other catalogues not in the trash, where the event could be listed.
    pub elsewhere: Vec<Named>,
}

pub fn event_detail<T: ReadTxn>(txn: &T, uuid: &str) -> Option<EventDetail> {
    let record = model::event(txn, uuid)?;
    let (start, stop) = (record.start.unix_nanos()?, record.stop.unix_nanos()?);
    let (start_text, stop_text) = range_text(start, stop);
    let listing = model::event_catalogues(txn, uuid);
    let (catalogues, elsewhere) = model::catalogues(txn)
        .into_iter()
        .map(|catalogue| Named {
            uuid: catalogue.uuid,
            name: catalogue.name,
        })
        .partition(|catalogue| listing.contains(&catalogue.uuid));
    Some(EventDetail {
        uuid: record.uuid,
        start: start_text,
        stop: stop_text,
        stop_time: time_text(stop),
        error: error_text(record.start_err, record.stop_err),
        duration: duration_text(stop - start),
        status: record.status.name().to_owned(),
        deleted: record.status == Status::Deleted,
        confidence: record.confidence,
        tags: record.tags,
        author: record.author,
        origin: record.origin.map(|id| origin_text(txn, &id)),
        comment: model::comment(txn, uuid),
        attributes: record
            .attributes
            .iter()
            .map(|(key, value)| (key.clone(), value_text(value)))
            .collect(),
        catalogues,
        elsewhere,
    })
}

/// The start in full, and the stop by its time alone when it falls on the same day.
pub fn range_text(start: i64, stop: i64) -> (String, String) {
    let stop_text = if date_text(start) == date_text(stop) {
        clock_text(stop)
    } else {
        time_text(stop)
    };
    (time_text(start), stop_text)
}

fn error_text(start: Option<f64>, stop: Option<f64>) -> Option<String> {
    match (start, stop) {
        (Some(start), Some(stop)) if start != stop => Some(format!("±{start} s, ±{stop} s")),
        (Some(error), _) | (None, Some(error)) => Some(format!("±{error} s")),
        (None, None) => None,
    }
}

/// "Detector sw-finder 1.2"; an origin the room does not describe reads as its id.
fn origin_text<T: ReadTxn>(txn: &T, id: &str) -> String {
    let Some(origin) = model::origin(txn, id) else {
        return id.to_owned();
    };
    let kind = match origin.kind {
        OriginKind::Rule => "Rule",
        OriginKind::Detector => "Detector",
        OriginKind::Model => "Model",
        OriginKind::Import => "Import",
    };
    match origin.version {
        Some(version) => format!("{kind} {} {version}", origin.name),
        None => format!("{kind} {}", origin.name),
    }
}

#[cfg(test)]
mod tests {
    use yrs::Transact;

    use super::*;

    fn named(uuid: &str, name: &str) -> Named {
        Named {
            uuid: uuid.into(),
            name: name.into(),
        }
    }

    #[test]
    fn an_event_detail_names_its_origin_comment_and_catalogues() {
        let detail = event_detail(&crate::fixture::room().transact(), "e2").unwrap();
        assert_eq!(
            detail,
            EventDetail {
                uuid: "e2".into(),
                start: "2019-11-05 08:47:30".into(),
                stop: "08:47:42".into(),
                stop_time: "2019-11-05 08:47:42".into(),
                error: Some("±0.5 s".into()),
                duration: "12 s".into(),
                status: "preliminary".into(),
                deleted: false,
                confidence: Some(0.74),
                tags: vec!["sw".into()],
                author: None,
                origin: Some("Detector sw-finder 1.2".into()),
                comment: Some("Seen by the finder only".into()),
                attributes: vec![("spacecraft".into(), "MMS2".into())],
                catalogues: Vec::new(),
                elsewhere: vec![
                    named("c-static", "Bow shock crossings"),
                    named("c-loop", "Loop"),
                    named("c-smart", "Solitary waves"),
                    named("c-merged", "Wave trains"),
                ],
            }
        );
    }

    #[test]
    fn an_event_lists_the_catalogues_that_list_it_and_the_others() {
        let detail = event_detail(&crate::fixture::room().transact(), "e1").unwrap();
        assert_eq!(
            detail.catalogues,
            [named("c-static", "Bow shock crossings")]
        );
        assert_eq!(detail.elsewhere.len(), 3);
        assert_eq!(detail.author.as_deref(), Some("alexis.jeandet"));
    }

    #[test]
    fn a_trashed_event_says_so() {
        let detail = event_detail(&crate::fixture::room().transact(), "e4").unwrap();
        assert!(detail.deleted);
    }

    #[test]
    fn a_range_over_midnight_names_the_stop_day() {
        assert_eq!(
            range_text(crate::fixture::DAY0 - 1_000_000_000, crate::fixture::DAY0),
            (
                "2019-11-02 23:59:59".to_owned(),
                "2019-11-03 00:00:00".to_owned()
            )
        );
    }

    #[test]
    fn an_unknown_event_has_no_detail() {
        assert!(event_detail(&crate::fixture::room().transact(), "nope").is_none());
    }
}
