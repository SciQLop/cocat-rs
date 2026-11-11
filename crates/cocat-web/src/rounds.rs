//! Selection rounds as the page shows them: the server's JSON and the lines drawn from it.

use cocat_core::model::EventRecord;
use cocat_core::round::{Cost, cost, parse_budget};
use serde::Deserialize;
use serde_json::Value;

/// A round as `GET /api/rooms/{room}/rounds` lists it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Round {
    pub id: String,
    pub name: String,
    pub state: String,
    pub visibility: String,
    pub deadline: Option<String>,
    pub budget: Option<Value>,
    pub final_catalogue: Option<String>,
    pub proposers: Vec<String>,
    pub deciders: Vec<String>,
    pub proposals: Vec<Proposal>,
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

pub fn over_cap(cost: &Cost) -> bool {
    cost.total > cost.cap
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round() -> Round {
        serde_json::from_value(json!({
            "id": "r1", "project": "p", "name": "Spring", "state": "proposing",
            "visibility": "blind", "deadline": null, "budget": null,
            "final_catalogue": null, "proposers": ["a@x.org", "b@x.org"], "voters": [],
            "deciders": ["c@x.org"],
            "proposals": [{"author": "a@x.org", "doc": null}],
            "log": [{"at": "2026-11-11T10:00:00Z", "actor": null, "what": "created"}]
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
}
