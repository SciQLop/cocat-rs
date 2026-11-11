//! Selection rounds as the page shows them: the server's JSON and the lines drawn from it.

use cocat_core::model::EventRecord;
use cocat_core::round::review::{ReviewRow, progress, reviewed};
use cocat_core::round::{Cost, cost, parse_budget};
use serde::Deserialize;
use serde_json::{Map, Value};

/// A round as `GET /api/rooms/{room}/rounds` lists it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Round {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub state: String,
    pub revision: i64,
    pub visibility: String,
    pub deadline: Option<String>,
    pub budget: Option<Value>,
    pub final_catalogue: Option<String>,
    pub proposers: Vec<String>,
    pub voters: Vec<String>,
    pub deciders: Vec<String>,
    pub proposals: Vec<Proposal>,
    pub candidates: Option<Candidates>,
    pub log: Vec<LogEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Proposal {
    pub author: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LogEntry {
    pub at: String,
    pub actor: Option<String>,
    pub what: String,
    pub note: Option<String>,
}

/// The events under review, copied from a catalogue when the round was made.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Candidates {
    pub count: usize,
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Source {
    pub project: String,
    pub catalogue: String,
}

/// How far a person is through the events they are asked to review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    /// How many events any reviewer gave a verdict on, as far as the person may read.
    pub reviewed: usize,
}

impl Progress {
    pub fn of(rows: &[ReviewRow]) -> Self {
        let (done, total, _) = progress(rows);
        Self {
            done,
            total,
            reviewed: reviewed(rows),
        }
    }
}

impl Round {
    /// "alice@example.org: started", or "not yet".
    pub fn proposer_line(&self, email: &str) -> String {
        let started = self
            .proposals
            .iter()
            .any(|proposal| proposal.author == email);
        let status = if started { "started" } else { "not yet" };
        format!("{email}: {status}")
    }

    /// What the events cost against the round's budget; none when the round has no readable one.
    pub fn cost_of(&self, events: &[EventRecord]) -> Option<Cost> {
        let budget = parse_budget(self.budget.as_ref()?).ok()?;
        Some(cost(events, &budget))
    }
}

/// What the round asks of `me`, in one line; none when nothing is asked. The counts show
/// when `progress` is known.
pub fn your_part(round: &Round, me: &str, progress: Option<Progress>) -> Option<String> {
    let is = |people: &[String]| people.iter().any(|email| email == me);
    match round.state.as_str() {
        "proposing" if is(&round.proposers) => Some(propose_text(round.deadline.as_deref())),
        "proposing" if is(&round.deciders) => Some(format!(
            "Decide: {} of {} people have proposed",
            round.proposals.len(),
            round.proposers.len()
        )),
        "voting" if is(&round.voters) => Some(review_text(round, progress)),
        "voting" if is(&round.deciders) => Some(match progress {
            Some(progress) => format!(
                "Decide: {} of {} reviewed",
                progress.reviewed, progress.total
            ),
            None => "Decide once the reviews are in".to_owned(),
        }),
        _ => None,
    }
}

fn propose_text(deadline: Option<&str>) -> String {
    match deadline {
        Some(deadline) => format!("Propose your intervals before {deadline}"),
        None => "Propose your intervals".to_owned(),
    }
}

fn review_text(round: &Round, progress: Option<Progress>) -> String {
    match (round.kind.as_str(), progress, &round.candidates) {
        ("review", Some(progress), _) => {
            format!(
                "Review {} detections ({} done)",
                progress.total, progress.done
            )
        }
        ("review", None, Some(candidates)) => format!("Review {} detections", candidates.count),
        (_, Some(progress), _) => format!(
            "Review the proposals ({} of {} reviewed)",
            progress.done, progress.total
        ),
        _ => "Review the proposals".to_owned(),
    }
}

/// The rounds that ask something of `me`, those under way, and those decided.
pub fn group<'a>(
    rounds: &'a [Round],
    me: &str,
) -> (Vec<&'a Round>, Vec<&'a Round>, Vec<&'a Round>) {
    let finished = |round: &&Round| round.state == "decided";
    let asks = |round: &&Round| your_part(round, me, None).is_some();
    let (finished, open): (Vec<_>, Vec<_>) = rounds.iter().partition(finished);
    let (needs_you, in_progress) = open.into_iter().partition(asks);
    (needs_you, in_progress, finished)
}

/// A round's state as a person says it: reviewers are "reviewing", not "voting".
pub fn state_words<'a>(kind: &str, state: &'a str) -> &'a str {
    match (kind, state) {
        ("review", "voting") => "reviewing",
        _ => state,
    }
}

/// The round a final catalogue was decided in, from the stamp on its `round` attribute.
pub fn decided_in(attributes: &Map<String, Value>) -> Option<(String, String)> {
    let stamp = match attributes.get("round")? {
        Value::String(text) => serde_json::from_str(text).ok()?,
        object => object.clone(),
    };
    let text = |key: &str| Some(stamp.get(key)?.as_str()?.to_owned());
    Some((text("cocat:round")?, text("cocat:name")?))
}

pub fn over_cap(cost: &Cost) -> bool {
    cost.total > cost.cap
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round() -> Round {
        serde_json::from_value(json!({
            "id": "r1", "project": "p", "name": "Spring", "kind": "downlink", "state": "proposing",
            "revision": 1, "visibility": "blind", "deadline": null, "budget": null,
            "final_catalogue": null, "proposers": ["a@x.org", "b@x.org"], "voters": ["v@x.org"],
            "deciders": ["c@x.org"],
            "proposals": [{"author": "a@x.org", "doc": null}], "candidates": null,
            "log": [{"at": "2026-11-11T10:00:00Z", "actor": null, "what": "created", "note": null}]
        }))
        .unwrap()
    }

    fn cost_of(total: f64, cap: f64) -> Cost {
        Cost {
            total,
            cap,
            notation: "Gbit".into(),
            unpriced: vec![],
        }
    }

    #[test]
    fn a_proposer_line_says_whether_they_started() {
        assert_eq!(round().proposer_line("a@x.org"), "a@x.org: started");
        assert_eq!(round().proposer_line("b@x.org"), "b@x.org: not yet");
    }

    #[test]
    fn the_budget_line_turns_red_over_the_cap() {
        assert!(over_cap(&cost_of(5.5, 5.0)));
        assert!(!over_cap(&cost_of(5.0, 5.0)));
        assert!(!over_cap(&cost_of(1.0, 5.0)));
    }

    #[test]
    fn a_round_without_a_budget_has_no_cost() {
        assert_eq!(round().cost_of(&[]), None);
    }

    fn in_state(kind: &str, state: &str) -> Round {
        Round {
            kind: kind.into(),
            state: state.into(),
            ..round()
        }
    }

    fn progress_of(done: usize, total: usize, reviewed: usize) -> Option<Progress> {
        Some(Progress {
            done,
            total,
            reviewed,
        })
    }

    #[test]
    fn your_part_tells_each_role_what_to_do() {
        let dated = Round {
            deadline: Some("2026-12-01".into()),
            ..round()
        };
        let mut detections = in_state("review", "voting");
        detections.candidates = Some(Candidates {
            count: 12,
            source: Source {
                project: "p".into(),
                catalogue: "c".into(),
            },
        });
        let proposals = in_state("downlink", "voting");
        let say = |round: &Round, me, progress| your_part(round, me, progress);
        assert_eq!(
            say(&dated, "a@x.org", None).as_deref(),
            Some("Propose your intervals before 2026-12-01")
        );
        assert_eq!(
            say(&round(), "a@x.org", None).as_deref(),
            Some("Propose your intervals")
        );
        assert_eq!(
            say(&detections, "v@x.org", progress_of(3, 12, 0)).as_deref(),
            Some("Review 12 detections (3 done)")
        );
        assert_eq!(
            say(&detections, "v@x.org", None).as_deref(),
            Some("Review 12 detections")
        );
        assert_eq!(
            say(&proposals, "v@x.org", progress_of(2, 9, 0)).as_deref(),
            Some("Review the proposals (2 of 9 reviewed)")
        );
        assert_eq!(
            say(&round(), "c@x.org", None).as_deref(),
            Some("Decide: 1 of 2 people have proposed")
        );
        assert_eq!(
            say(&proposals, "c@x.org", progress_of(0, 9, 4)).as_deref(),
            Some("Decide: 4 of 9 reviewed")
        );
        assert_eq!(say(&round(), "nobody@x.org", None), None);
        assert_eq!(say(&in_state("downlink", "decided"), "c@x.org", None), None);
    }

    #[test]
    fn rounds_are_grouped_by_what_they_need_from_me() {
        let rounds = [
            round(),
            in_state("downlink", "voting"),
            in_state("downlink", "decided"),
        ];
        let ids = |rounds: Vec<&Round>| rounds.iter().map(|r| r.state.clone()).collect::<Vec<_>>();
        let (needs, progress, finished) = group(&rounds, "a@x.org");
        assert_eq!(ids(needs), ["proposing"]);
        assert_eq!(ids(progress), ["voting"]);
        assert_eq!(ids(finished), ["decided"]);
    }

    #[test]
    fn a_review_round_says_reviewing() {
        assert_eq!(state_words("review", "voting"), "reviewing");
        assert_eq!(state_words("downlink", "voting"), "voting");
        assert_eq!(state_words("review", "proposing"), "proposing");
    }

    #[test]
    fn a_final_catalogue_names_its_round() {
        let stamp = json!({"cocat:round": "r1", "cocat:name": "Spring"});
        let attributes = |value: Value| {
            let mut map = Map::new();
            map.insert("round".into(), value);
            map
        };
        let expected = Some(("r1".to_owned(), "Spring".to_owned()));
        assert_eq!(decided_in(&attributes(json!(stamp.to_string()))), expected);
        assert_eq!(decided_in(&attributes(stamp)), expected);
        assert_eq!(decided_in(&attributes(json!("not json"))), None);
        assert_eq!(decided_in(&Map::new()), None);
    }
}
