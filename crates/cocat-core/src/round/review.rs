//! The rules of reviewing a round, shared by every client: one row per candidate or proposed
//! event with what reviewers said, how far the caller got, the majority default, and the
//! events of the final catalogue.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use yrs::ReadTxn;

use crate::model::{self, EventRecord, Status};
use crate::time::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Accept,
    Reject,
    Unsure,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReviewRow {
    pub event: EventRecord,
    /// The proposer's email; `None` for a candidate.
    pub proposal: Option<String>,
    pub accepts: u32,
    pub rejects: u32,
    pub unsure: u32,
    /// What the caller said in the round's current revision.
    pub mine: Option<Verdict>,
    /// A verdict on an event that is no longer a candidate or proposed; `event` then holds
    /// nothing but its uuid.
    pub set_aside: bool,
}

#[derive(Deserialize)]
struct Answer {
    revision: i64,
    events: Vec<Tally>,
}

#[derive(Deserialize)]
struct Tally {
    event: String,
    accept: u32,
    reject: u32,
    unsure: u32,
    #[serde(default)]
    by: Vec<Given>,
}

#[derive(Deserialize)]
struct Given {
    reviewer: String,
    verdict: Verdict,
    revision: i64,
}

fn tally_row(
    event: EventRecord,
    proposal: Option<String>,
    tally: Option<&Tally>,
    me: &str,
    revision: i64,
) -> ReviewRow {
    let mine = tally.and_then(|tally| {
        tally
            .by
            .iter()
            .find(|given| given.reviewer == me && given.revision == revision)
            .map(|given| given.verdict)
    });
    ReviewRow {
        event,
        proposal,
        accepts: tally.map_or(0, |tally| tally.accept),
        rejects: tally.map_or(0, |tally| tally.reject),
        unsure: tally.map_or(0, |tally| tally.unsure),
        mine,
        set_aside: false,
    }
}

fn stub(uuid: &str) -> EventRecord {
    let epoch = Timestamp::parse("1970-01-01T00:00:00Z").expect("a valid instant");
    EventRecord::new(uuid, epoch, epoch)
}

/// The rows of the candidates (proposal `None`) and proposals (their author's email), in the
/// order given, then one set-aside row per verdict on an event not among them.
/// `verdicts` is the answer of `GET …/verdicts`; `me` is the caller's email.
pub fn rows(
    events_by_proposal: &[(Option<String>, Vec<EventRecord>)],
    verdicts: &Value,
    me: &str,
) -> Result<Vec<ReviewRow>, serde_json::Error> {
    let answer: Answer = serde_json::from_value(verdicts.clone())?;
    let tallies: HashMap<&str, &Tally> = answer
        .events
        .iter()
        .map(|tally| (tally.event.as_str(), tally))
        .collect();
    let current: Vec<ReviewRow> = events_by_proposal
        .iter()
        .flat_map(|(proposal, events)| events.iter().map(move |event| (proposal, event)))
        .map(|(proposal, event)| {
            let tally = tallies.get(event.uuid.as_str()).copied();
            tally_row(event.clone(), proposal.clone(), tally, me, answer.revision)
        })
        .collect();
    let known: HashSet<&str> = current.iter().map(|row| row.event.uuid.as_str()).collect();
    let set_aside: Vec<ReviewRow> = answer
        .events
        .iter()
        .filter(|tally| !known.contains(tally.event.as_str()))
        .map(|tally| ReviewRow {
            set_aside: true,
            ..tally_row(stub(&tally.event), None, Some(tally), me, answer.revision)
        })
        .collect();
    Ok([current, set_aside].concat())
}

/// The first row from `from` on, wrapping round to the start, that the caller has not
/// reviewed.
pub fn next_unreviewed(rows: &[ReviewRow], from: usize) -> Option<usize> {
    (from..rows.len())
        .chain(0..from.min(rows.len()))
        .find(|&index| !rows[index].set_aside && rows[index].mine.is_none())
}

/// `(done, total, set_aside)`: the current rows the caller reviewed, all current rows, and
/// the verdicts on events that are no longer current.
pub fn progress(rows: &[ReviewRow]) -> (usize, usize, usize) {
    let set_aside = rows.iter().filter(|row| row.set_aside).count();
    let done = rows
        .iter()
        .filter(|row| !row.set_aside && row.mine.is_some())
        .count();
    (done, rows.len() - set_aside, set_aside)
}

/// How many current rows any reviewer gave a verdict on, as far as the caller may read.
pub fn reviewed(rows: &[ReviewRow]) -> usize {
    rows.iter()
        .filter(|row| !row.set_aside && row.accepts + row.rejects + row.unsure > 0)
        .count()
}

/// Which rows more reviewers accepted than rejected; a tie keeps nothing.
pub fn majority(rows: &[ReviewRow]) -> Vec<bool> {
    rows.iter()
        .map(|row| !row.set_aside && row.accepts > row.rejects)
        .collect()
}

fn final_event(row: &ReviewRow) -> EventRecord {
    let mut event = row.event.clone();
    event.uuid = Uuid::new_v4().to_string();
    let proposed_by: Vec<&String> = row.proposal.iter().collect();
    let added: Map<String, Value> = [
        ("candidate", json!(row.event.uuid)),
        ("proposed_by", json!(proposed_by)),
        ("accepts", json!(row.accepts)),
        ("rejects", json!(row.rejects)),
        ("unsure", json!(row.unsure)),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value))
    .collect();
    event.attributes.extend(added);
    event
}

/// The final catalogue's events: copies of the picked rows under fresh uuids, noting the
/// original uuid, who proposed it and what reviewers said.
pub fn final_events(rows: &[ReviewRow], picked: &[bool]) -> Vec<EventRecord> {
    rows.iter()
        .zip(picked)
        .filter(|(row, picked)| **picked && !row.set_aside)
        .map(|(row, _)| final_event(row))
        .collect()
}

/// The events a proposal document puts forward: those its catalogues list, outside the trash,
/// each once, in time order.
pub fn proposal_events<T: ReadTxn>(txn: &T) -> Vec<EventRecord> {
    let listed: BTreeSet<String> = model::catalogues(txn)
        .iter()
        .flat_map(|catalogue| model::listed_events(txn, &catalogue.uuid))
        .collect();
    let mut events: Vec<EventRecord> = listed
        .iter()
        .filter_map(|uuid| model::event(txn, uuid))
        .filter(|event| event.status != Status::Deleted)
        .collect();
    events.sort_by(|left, right| (left.start, &left.uuid).cmp(&(right.start, &right.uuid)));
    events
}

/// The same uuid for a round on every client, so deciding again rewrites one catalogue.
pub fn final_catalogue_uuid(round_id: &str) -> String {
    let name = format!("cocat:round:{round_id}");
    Uuid::new_v5(&Uuid::NAMESPACE_URL, name.as_bytes()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(uuid: &str) -> EventRecord {
        let at = Timestamp::parse("2020-01-01T00:00:00Z").unwrap();
        EventRecord::new(uuid, at, at)
    }

    fn verdicts() -> Value {
        json!({"revision": 2, "events": [
            {"event": "a", "accept": 2, "reject": 0, "unsure": 1, "by": [
                {"reviewer": "me@x.org", "verdict": "accept", "revision": 2},
                {"reviewer": "bo@x.org", "verdict": "unsure", "revision": 2}]},
            {"event": "b", "accept": 0, "reject": 1, "unsure": 0, "by": [
                {"reviewer": "me@x.org", "verdict": "reject", "revision": 1}]},
            {"event": "gone", "accept": 1, "reject": 0, "unsure": 0}
        ]})
    }

    fn sample() -> Vec<ReviewRow> {
        let events = [
            (None, vec![event("a"), event("b")]),
            (Some("ada@x.org".to_owned()), vec![event("c")]),
        ];
        rows(&events, &verdicts(), "me@x.org").unwrap()
    }

    #[test]
    fn a_row_holds_the_counts_and_my_verdict_of_this_revision() {
        let rows = sample();

        let seen: Vec<_> = rows
            .iter()
            .map(|r| (r.accepts, r.rejects, r.unsure, r.mine))
            .collect();
        assert_eq!(
            seen[..3],
            [
                (2, 0, 1, Some(Verdict::Accept)),
                (0, 1, 0, None),
                (0, 0, 0, None)
            ]
        );
        assert_eq!(rows[2].proposal.as_deref(), Some("ada@x.org"));
    }

    #[test]
    fn a_verdict_on_an_event_no_longer_there_is_set_aside() {
        let rows = sample();

        assert_eq!(rows.len(), 4);
        assert!(rows[3].set_aside);
        assert_eq!(rows[3].event.uuid, "gone");
        assert_eq!(progress(&rows), (1, 3, 1));
    }

    #[test]
    fn the_next_unreviewed_row_skips_reviewed_and_set_aside_rows_and_wraps() {
        let rows = sample();

        assert_eq!(next_unreviewed(&rows, 0), Some(1));
        assert_eq!(next_unreviewed(&rows, 2), Some(2));
        assert_eq!(next_unreviewed(&rows, 3), Some(1));
        assert_eq!(next_unreviewed(&rows[..1], 0), None);
    }

    #[test]
    fn reviewed_counts_current_rows_anyone_gave_a_verdict_on() {
        assert_eq!(reviewed(&sample()), 2);
        let mut rows = sample();
        rows[2].unsure = 1;
        assert_eq!(reviewed(&rows), 3);
    }

    #[test]
    fn a_final_event_keeps_every_field_of_its_candidate() {
        let mut rows = sample();
        rows[0].event.tags = vec!["t".to_owned()];
        rows[0].event.status = crate::model::Status::Reviewed;
        rows[0].event.confidence = Some(0.5);

        let kept = &final_events(&rows, &[true, false, false, false])[0];

        assert_eq!(kept.tags, ["t"]);
        assert_eq!(kept.status, crate::model::Status::Reviewed);
        assert_eq!(kept.confidence, Some(0.5));
    }

    #[test]
    fn a_majority_needs_more_accepts_than_rejects() {
        let mut rows = sample();
        rows[1].accepts = 1;

        assert_eq!(majority(&rows), [true, false, false, false]);
    }

    #[test]
    fn final_events_get_fresh_uuids_and_note_where_they_came_from() {
        let rows = sample();

        let events = final_events(&rows, &[true, false, true, true]);

        assert_eq!(events.len(), 2);
        assert_ne!(events[0].uuid, "a");
        assert_eq!(events[0].attributes["candidate"], json!("a"));
        assert_eq!(events[0].attributes["proposed_by"], json!([]));
        assert_eq!(events[0].attributes["accepts"], json!(2));
        assert_eq!(events[0].attributes["unsure"], json!(1));
        assert_eq!(events[1].attributes["proposed_by"], json!(["ada@x.org"]));
    }

    #[test]
    fn a_proposal_puts_forward_the_live_events_its_catalogues_list() {
        use crate::model::{self, NewCatalogue};
        use yrs::{Doc, Transact};

        let doc = Doc::new();
        {
            let mut txn = doc.transact_mut();
            let at = |text: &str| Timestamp::parse(text).unwrap();
            for (uuid, start) in [
                ("late", "2020-03-01T00:00:00Z"),
                ("early", "2020-01-01T00:00:00Z"),
                ("trashed", "2020-02-01T00:00:00Z"),
                ("loose", "2020-02-01T00:00:00Z"),
                ("hidden", "2020-02-01T00:00:00Z"),
            ] {
                model::create_event(&mut txn, &EventRecord::new(uuid, at(start), at(start)))
                    .unwrap();
            }
            model::delete_event(&mut txn, "trashed");
            for uuid in ["one", "two", "gone"] {
                let catalogue = NewCatalogue {
                    uuid: uuid.into(),
                    name: uuid.into(),
                    ..NewCatalogue::default()
                };
                model::create_catalogue(&mut txn, &catalogue).unwrap();
            }
            let list = |uuids: &[&str]| uuids.iter().map(|u| (*u).to_owned()).collect::<Vec<_>>();
            model::add_catalogue_events(&mut txn, "one", &list(&["late", "trashed"]));
            model::add_catalogue_events(&mut txn, "two", &list(&["early", "late"]));
            model::add_catalogue_events(&mut txn, "gone", &list(&["hidden"]));
            model::delete_catalogue(&mut txn, "gone");
        }

        let events = proposal_events(&doc.transact());

        let uuids: Vec<&str> = events.iter().map(|event| event.uuid.as_str()).collect();
        assert_eq!(uuids, ["early", "late"]);
    }

    #[test]
    fn the_final_catalogue_uuid_depends_only_on_the_round() {
        let first = final_catalogue_uuid("11111111-1111-1111-1111-111111111111");

        assert_eq!(
            first,
            final_catalogue_uuid("11111111-1111-1111-1111-111111111111")
        );
        assert_ne!(
            first,
            final_catalogue_uuid("22222222-2222-2222-2222-222222222222")
        );
        assert_eq!(first.len(), 36);
    }
}
