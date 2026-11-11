//! Selection rounds as the page shows them: the server's JSON and the lines drawn from it.

use crate::sharing::Member;
use cocat_core::model::EventRecord;
use cocat_core::round::review::{ReviewRow, progress, reviewed};
use cocat_core::round::{
    Budget, Cost, Quantity, budget_json as core_budget_json, cost, parse_budget, parse_unit,
};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    Required,
    Optional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Defaults {
    Nobody,
    Writers,
}

/// A starting point for the new-round form: what each kind of round usually needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    pub label: &'static str,
    pub kind: &'static str,
    pub visibility: &'static str,
    pub budget_form: bool,
    pub candidates: Need,
    pub default_proposers: Defaults,
}

pub const PRESETS: [Preset; 3] = [
    Preset {
        label: "Review detections",
        kind: "review",
        visibility: "open",
        budget_form: false,
        candidates: Need::Required,
        default_proposers: Defaults::Nobody,
    },
    Preset {
        label: "Selective downlink",
        kind: "downlink",
        visibility: "open",
        budget_form: true,
        candidates: Need::Optional,
        default_proposers: Defaults::Writers,
    },
    Preset {
        label: "Labelling",
        kind: "labelling",
        visibility: "blind",
        budget_form: false,
        candidates: Need::Optional,
        default_proposers: Defaults::Writers,
    },
];

/// What creating a round still has to do: a retry after a failed members call must not make
/// a second round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    Create,
    SetPeople(String),
}

pub fn next_step(created: Option<String>) -> Next {
    created.map_or(Next::Create, Next::SetPeople)
}

// simplify: guesses the import race from the words "not found" in the server's 404; give
// the API an error code for a missing candidates catalogue to match on instead.
/// A failed creation in words; the server's 404 when the candidates are a catalogue just
/// imported means its catalogue has not reached it yet.
pub fn creation_failure(message: &str, just_imported: bool) -> String {
    if just_imported && message.contains("not found") {
        return "The import has not reached the server yet; try again in a moment.".to_owned();
    }
    message.to_owned()
}

/// The People table's column for the voter role, as a verb like "Proposes" and "Decides".
pub fn voter_verb(kind: &str) -> &'static str {
    if kind == "review" { "Reviews" } else { "Votes" }
}

/// What the voter role is called: reviewers judge detections, voters pick among proposals.
pub fn voter_title(kind: &str) -> &'static str {
    if kind == "review" {
        "Reviewers"
    } else {
        "Voters"
    }
}

/// Owners and editors may start a round, but not in a personal or a public project.
pub fn can_start(writer: bool, personal: bool, public: bool) -> bool {
    writer && !personal && !public
}

pub const DATA_UNITS: [&str; 10] = [
    "byte", "kbyte", "Mbyte", "Gbyte", "Kibyte", "Mibyte", "Gibyte", "bit", "kbit", "Mbit",
];
pub const TIMES: [&str; 3] = ["s", "min", "h"];

/// One project member's roles in the round being made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seat {
    pub email: String,
    pub can_decide: bool,
    pub proposer: bool,
    pub voter: bool,
    pub decider: bool,
}

/// What a mode costs: `amount` of `unit` per `per`, for events whose attribute is `value`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RateRow {
    pub value: String,
    pub amount: String,
    pub unit: String,
    pub per: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetForm {
    pub attribute: String,
    pub cap: String,
    pub cap_unit: String,
    pub rates: Vec<RateRow>,
}

impl Default for BudgetForm {
    fn default() -> Self {
        Self {
            attribute: String::new(),
            cap: String::new(),
            cap_unit: "Gibyte".into(),
            rates: Vec::new(),
        }
    }
}

fn quantity(amount: &str, notation: &str) -> Result<Quantity, String> {
    let value = amount
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .ok_or_else(|| format!("{amount:?} is not a number"))?;
    let unit = parse_unit(notation).map_err(|error| error.to_string())?;
    Ok(Quantity {
        value,
        notation: notation.to_owned(),
        unit,
    })
}

fn rate(row: &RateRow) -> Result<(String, Quantity), String> {
    let notation = format!("{}/{}", row.unit, row.per);
    Ok((
        row.value.trim().to_owned(),
        quantity(&row.amount, &notation)?,
    ))
}

/// The budget as the spec's JSON-LD, checked the way the server will check it.
pub fn budget_json(form: &BudgetForm) -> Result<Value, String> {
    let attribute = form.attribute.trim().to_owned();
    if attribute.is_empty() {
        return Err("Name the event attribute the budget prices.".to_owned());
    }
    let budget = Budget {
        attribute,
        cap: quantity(&form.cap, &form.cap_unit)?,
        rates: form.rates.iter().map(rate).collect::<Result<_, _>>()?,
    };
    let made = core_budget_json(&budget);
    parse_budget(&made).map_err(|error| error.to_string())?;
    Ok(made)
}

/// The new-round form's content: a preset's defaults, then the visitor's edits.
#[derive(Debug, Clone, PartialEq)]
pub struct NewRound {
    pub name: String,
    pub preset: Preset,
    pub visibility: &'static str,
    pub deadline: String,
    pub budget: Option<BudgetForm>,
    pub candidates: Option<Source>,
    pub seats: Vec<Seat>,
}

impl NewRound {
    pub fn new(preset: Preset, members: &[Member], me: &str) -> Self {
        let propose = |writer| writer && preset.default_proposers == Defaults::Writers;
        let seats = members
            .iter()
            .map(|member| Seat {
                email: member.email.clone(),
                can_decide: member.role.can_write(),
                proposer: propose(member.role.can_write()),
                voter: member.role.can_write(),
                decider: member.email == me,
            })
            .collect();
        Self {
            name: String::new(),
            preset,
            visibility: preset.visibility,
            deadline: String::new(),
            budget: preset.budget_form.then(BudgetForm::default),
            candidates: None,
            seats,
        }
    }

    /// Another preset's defaults, keeping what the visitor typed and picked.
    pub fn with_preset(self, preset: Preset, members: &[Member], me: &str) -> Self {
        let fresh = Self::new(preset, members, me);
        Self {
            name: self.name,
            deadline: self.deadline,
            candidates: self.candidates,
            budget: self.budget.filter(|_| preset.budget_form).or(fresh.budget),
            ..fresh
        }
    }

    pub fn kind(&self) -> &'static str {
        self.preset.kind
    }

    pub fn visibility(&self) -> &'static str {
        self.visibility
    }

    /// The body of `POST /api/rooms/{room}/rounds`, or what is missing.
    pub fn request(&self) -> Result<Value, String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("Give the round a name.".to_owned());
        }
        if self.preset.candidates == Need::Required && self.candidates.is_none() {
            return Err("Pick the catalogue of detections to review.".to_owned());
        }
        if self.candidates.is_none() && self.chosen(|seat| seat.proposer).is_empty() {
            return Err("Pick at least one proposer, or a catalogue of candidates.".to_owned());
        }
        let mut body = serde_json::json!({
            "name": name, "kind": self.kind(), "visibility": self.visibility(),
        });
        if let Some(source) = &self.candidates {
            body["candidates"] =
                serde_json::json!({"project": source.project, "catalogue": source.catalogue});
        }
        if !self.deadline.trim().is_empty() {
            body["deadline"] = Value::String(self.deadline.trim().to_owned());
        }
        if let Some(budget) = &self.budget {
            body["budget"] = budget_json(budget)?;
        }
        Ok(body)
    }

    /// The body of `PUT .../members`.
    pub fn members_request(&self) -> Value {
        serde_json::json!({
            "proposers": self.chosen(|seat| seat.proposer),
            "voters": self.chosen(|seat| seat.voter),
            "deciders": self.chosen(|seat| seat.decider),
        })
    }

    fn chosen(&self, role: fn(&Seat) -> bool) -> Vec<&str> {
        self.seats
            .iter()
            .filter(|seat| role(seat))
            .map(|seat| seat.email.as_str())
            .collect()
    }
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

    fn members() -> Vec<Member> {
        let member = |email: &str, role: &str| {
            serde_json::from_value(json!({"user_id": email, "email": email, "role": role})).unwrap()
        };
        vec![
            member("me@x.org", "owner"),
            member("ed@x.org", "editor"),
            member("view@x.org", "viewer"),
        ]
    }

    fn form(preset: usize) -> NewRound {
        NewRound::new(PRESETS[preset], &members(), "me@x.org")
    }

    fn emails(seats: &[Seat], role: fn(&Seat) -> bool) -> Vec<&str> {
        seats
            .iter()
            .filter(|seat| role(seat))
            .map(|seat| seat.email.as_str())
            .collect()
    }

    #[test]
    fn each_preset_fills_the_form() {
        let review = form(0);
        assert_eq!((review.kind(), review.visibility()), ("review", "open"));
        assert!(review.budget.is_none());
        assert_eq!(review.preset.candidates, Need::Required);
        assert!(emails(&review.seats, |seat| seat.proposer).is_empty());

        let downlink = form(1);
        assert_eq!(
            (downlink.kind(), downlink.visibility()),
            ("downlink", "open")
        );
        assert!(downlink.budget.is_some());
        assert_eq!(downlink.preset.candidates, Need::Optional);
        assert_eq!(
            emails(&downlink.seats, |seat| seat.proposer),
            ["me@x.org", "ed@x.org"]
        );

        let labelling = form(2);
        assert_eq!(
            (labelling.kind(), labelling.visibility()),
            ("labelling", "blind")
        );
        assert!(labelling.budget.is_none());
        assert_eq!(
            emails(&labelling.seats, |seat| seat.proposer),
            ["me@x.org", "ed@x.org"]
        );
        assert_eq!(
            emails(&labelling.seats, |seat| seat.voter),
            ["me@x.org", "ed@x.org"]
        );
        assert_eq!(emails(&labelling.seats, |seat| seat.decider), ["me@x.org"]);
    }

    fn budget_form() -> BudgetForm {
        BudgetForm {
            attribute: "mode".into(),
            cap: "5".into(),
            cap_unit: "Gbit".into(),
            rates: vec![RateRow {
                value: "burst".into(),
                amount: "2".into(),
                unit: "Mibyte".into(),
                per: "min".into(),
            }],
        }
    }

    #[test]
    fn the_budget_form_makes_the_spec_json_ld() {
        let made = budget_json(&budget_form()).unwrap();
        assert_eq!(made["@type"], "cocat:Budget");
        assert_eq!(made["cocat:attribute"], "mode");
        assert_eq!(made["cocat:cap"]["qudt:numericValue"], 5.0);
        assert_eq!(made["cocat:cap"]["qudt:hasUnit"]["skos:notation"], "Gbit");
        let rate = &made["cocat:rates"][0];
        assert_eq!(rate["cocat:value"], "burst");
        assert_eq!(
            rate["cocat:rate"]["qudt:hasUnit"]["skos:notation"],
            "Mibyte/min"
        );
        assert!(parse_budget(&made).is_ok());
    }

    #[test]
    fn a_budget_with_a_rate_in_the_wrong_unit_is_refused_before_sending() {
        let wrong = |edit: fn(&mut BudgetForm)| {
            let mut form = budget_form();
            edit(&mut form);
            budget_json(&form).is_err()
        };
        assert!(wrong(|form| form.rates[0].per = "fortnight".into()));
        assert!(wrong(|form| form.rates[0].unit = "MB".into()));
        assert!(wrong(|form| form.cap_unit = "Mibyte/min".into()));
        assert!(wrong(|form| form.cap = "lots".into()));
        assert!(wrong(|form| form.attribute = " ".into()));
    }

    #[test]
    fn a_round_is_sent_only_when_it_can_work() {
        let source = Source {
            project: "p".into(),
            catalogue: "c".into(),
        };
        let mut round = form(2);
        assert!(round.request().is_err());
        round.name = " Spring ".into();
        let body = round.request().unwrap();
        assert_eq!(body["name"], "Spring");
        assert_eq!(body["kind"], "labelling");
        assert!(body.get("candidates").is_none());

        let mut review = form(0);
        review.name = "Detections".into();
        assert!(review.request().is_err());
        review.candidates = Some(source);
        let body = review.request().unwrap();
        assert_eq!(
            body["candidates"],
            json!({"project": "p", "catalogue": "c"})
        );

        let mut nobody = form(2);
        nobody.name = "x".into();
        nobody
            .seats
            .iter_mut()
            .for_each(|seat| seat.proposer = false);
        assert!(nobody.request().is_err());
    }

    #[test]
    fn only_editors_can_decide_and_the_members_body_lists_each_role() {
        let round = form(2);
        let editable: Vec<_> = round.seats.iter().map(|seat| seat.can_decide).collect();
        assert_eq!(editable, [true, true, false]);
        let body = round.members_request();
        assert_eq!(body["proposers"], json!(["me@x.org", "ed@x.org"]));
        assert_eq!(body["voters"], json!(["me@x.org", "ed@x.org"]));
        assert_eq!(body["deciders"], json!(["me@x.org"]));
    }

    #[test]
    fn changing_preset_keeps_what_was_typed() {
        let mut typed = form(1);
        typed.name = "Spring".into();
        typed.budget.as_mut().unwrap().cap = "7".into();
        let source = Source {
            project: "p".into(),
            catalogue: "c".into(),
        };
        typed.candidates = Some(source.clone());
        let changed = typed.with_preset(PRESETS[2], &members(), "me@x.org");
        assert_eq!(changed.name, "Spring");
        assert_eq!(changed.candidates, Some(source));
        assert_eq!(changed.kind(), "labelling");
        assert!(changed.budget.is_none());
    }

    #[test]
    fn the_default_cap_unit_is_one_of_the_listed_units() {
        assert!(DATA_UNITS.contains(&BudgetForm::default().cap_unit.as_str()));
        assert!(
            budget_json(&BudgetForm {
                attribute: "m".into(),
                cap: "1".into(),
                ..BudgetForm::default()
            })
            .is_ok()
        );
    }

    #[test]
    fn a_retry_after_a_failed_members_call_does_not_make_a_second_round() {
        assert_eq!(next_step(None), Next::Create);
        assert_eq!(next_step(Some("r1".into())), Next::SetPeople("r1".into()));
    }

    #[test]
    fn a_missing_catalogue_after_an_import_says_to_wait() {
        let not_found = "Catalogue c not found";
        assert!(creation_failure(not_found, true).contains("try again"));
        assert_eq!(creation_failure(not_found, false), not_found);
        assert_eq!(creation_failure("boom", true), "boom");
    }

    #[test]
    fn the_visitor_may_flip_the_visibility_and_a_preset_resets_it() {
        let mut round = form(2);
        round.visibility = "open";
        assert_eq!(round.request().unwrap_err(), "Give the round a name.");
        round.name = "x".into();
        assert_eq!(round.request().unwrap()["visibility"], "open");
        let changed = round.with_preset(PRESETS[0], &members(), "me@x.org");
        assert_eq!(changed.visibility(), "open");
        let blind = changed.with_preset(PRESETS[2], &members(), "me@x.org");
        assert_eq!(blind.visibility(), "blind");
    }

    #[test]
    fn the_people_columns_are_verbs() {
        assert_eq!(voter_verb("review"), "Reviews");
        assert_eq!(voter_verb("labelling"), "Votes");
    }

    #[test]
    fn the_voter_role_is_called_reviewer_in_review_rounds() {
        assert_eq!(voter_title("review"), "Reviewers");
        assert_eq!(voter_title("downlink"), "Voters");
        assert_eq!(voter_title("labelling"), "Voters");
    }

    #[test]
    fn rounds_start_in_personal_or_public_projects_never() {
        assert!(can_start(true, false, false));
        assert!(!can_start(false, false, false));
        assert!(!can_start(true, true, false));
        assert!(!can_start(true, false, true));
    }
}
