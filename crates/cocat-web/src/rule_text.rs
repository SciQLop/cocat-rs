//! A smart catalogue's rule in words, for the catalogue's header.

use cocat_core::model::Hop;
use cocat_core::rules::{self, Op, Predicate, RuleError, Set, intervals::seconds, relative};
use cocat_core::time::Timestamp;
use serde_json::Value;

use crate::format::{date_text, duration_text, person_text, value_text};

/// The rule as a sentence; `name` gives a catalogue's name from its uuid.
pub fn describe(rule: &Value, name: &dyn Fn(&str) -> Option<String>) -> Result<String, RuleError> {
    Ok(Words { name }.set(&rules::parse(rule)?))
}

struct Words<'n> {
    name: &'n dyn Fn(&str) -> Option<String>,
}

impl Words<'_> {
    fn set(&self, set: &Set) -> String {
        match set {
            Set::All { .. } => "every event".to_owned(),
            Set::Catalogue { catalogue } => match (self.name)(catalogue) {
                Some(name) => format!("“{name}”"),
                None => format!("catalogue {catalogue}"),
            },
            Set::Filter { filter, predicate } => {
                let subject = match filter.as_ref() {
                    Set::All { .. } => "events".to_owned(),
                    other => self.operand(other),
                };
                format!("{subject} where {}", self.predicate(predicate))
            }
            Set::Merge { merge, gap } => {
                format!("{}, merged within {}", self.set(merge), length(*gap))
            }
            Set::Intersect { intersect: (a, b) } => {
                format!("{} overlapping {}", self.operand(a), self.operand(b))
            }
            Set::Subtract { subtract: (a, b) } => {
                format!("{} minus {}", self.operand(a), self.operand(b))
            }
            Set::Union { union } => union
                .iter()
                .map(|set| self.operand(set))
                .collect::<Vec<_>>()
                .join(" or "),
            Set::Shift { shift, by } => format!("{}, shifted by {}", self.set(shift), length(*by)),
            Set::Tile { tile, width } => {
                format!("{}, cut into {} tiles", self.set(tile), length(*width))
            }
            Set::Window {
                window: (a, b),
                within,
            } => format!(
                "{} within {} of {}",
                self.operand(a),
                length(*within),
                self.operand(b)
            ),
            Set::Closest { closest: (a, b) } => format!(
                "{}, with the gap to the nearest of {}",
                self.operand(a),
                self.operand(b)
            ),
        }
    }

    /// A set inside another, bracketed unless it is one word.
    fn operand(&self, set: &Set) -> String {
        match set {
            Set::All { .. } | Set::Catalogue { .. } => self.set(set),
            _ => format!("({})", self.set(set)),
        }
    }

    fn predicate(&self, predicate: &Predicate) -> String {
        match predicate {
            Predicate::Cmp(field, op, value) => {
                format!(
                    "{} {} {}",
                    field_name(field),
                    symbol(*op),
                    operand_text(field, value)
                )
            }
            Predicate::Match(field, pattern) => {
                format!("{} matches /{pattern}/", field_name(field))
            }
            Predicate::Has(field) => format!("has {}", field_name(field)),
            Predicate::Contains(field, value) => match field.as_str() {
                "tags" => format!("tag {}", value_text(value)),
                "products" => format!("product {}", value_text(value)),
                other => format!("{} contains {}", field_name(other), value_text(value)),
            },
            Predicate::Not(inner) => format!("not {}", self.clause(inner)),
            Predicate::And(clauses) => self.clauses(clauses, " and "),
            Predicate::Or(clauses) => self.clauses(clauses, " or "),
            Predicate::InCatalogue(Some(catalogue)) => {
                format!(
                    "in {}",
                    self.set(&Set::Catalogue {
                        catalogue: catalogue.clone()
                    })
                )
            }
            Predicate::InCatalogue(None) => "in no catalogue".to_owned(),
            Predicate::Text(word) => format!("mentions “{word}”"),
        }
    }

    fn clauses(&self, clauses: &[Predicate], joint: &str) -> String {
        clauses
            .iter()
            .map(|clause| self.clause(clause))
            .collect::<Vec<_>>()
            .join(joint)
    }

    /// A predicate inside another, bracketed when it joins several.
    fn clause(&self, predicate: &Predicate) -> String {
        match predicate {
            Predicate::And(_) | Predicate::Or(_) => format!("({})", self.predicate(predicate)),
            _ => self.predicate(predicate),
        }
    }
}

/// A compared value as people read it: a time a search wrote in nanoseconds as a date, a
/// duration in seconds as a length.
fn operand_text(field: &str, value: &Value) -> String {
    const TIMES: [&str; 3] = ["start", "stop", "created"];
    let number = value.as_f64();
    match (relative::offset_of(value), field, number) {
        (Some(offset), ..) => format!("now{offset}"),
        (None, "duration", Some(seconds)) => crate::format::duration_text((seconds * 1e9) as i64),
        (None, field, Some(_)) if TIMES.contains(&field) => value
            .as_i64()
            .map_or_else(|| value_text(value), crate::format::time_text),
        _ => value_text(value),
    }
}

fn field_name(field: &str) -> &str {
    field.strip_prefix('#').unwrap_or(field)
}

fn symbol(op: Op) -> &'static str {
    match op {
        Op::Eq => "=",
        Op::Ne => "≠",
        Op::Lt => "<",
        Op::Le => "≤",
        Op::Gt => ">",
        Op::Ge => "≥",
    }
}

fn length(seconds_value: f64) -> String {
    duration_text(seconds(seconds_value))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn words(rule: serde_json::Value) -> String {
        let name = |uuid: &str| (uuid == "c1").then(|| "All candidates".to_owned());
        describe(&rule, &name).unwrap()
    }

    #[test]
    fn a_merged_filter_reads_as_a_sentence() {
        let rule = json!({
            "merge": {
                "filter": {"all": true},
                "where": {"and": [{"contains": ["tags", "sw"]}, {"cmp": ["confidence", ">=", 0.7]}]}
            },
            "gap": 30
        });
        assert_eq!(
            words(rule),
            "events where tag sw and confidence ≥ 0.7, merged within 30 s"
        );
    }

    #[test]
    fn catalogues_and_attributes_read_by_name() {
        let rule = json!({
            "filter": {"catalogue": "c1"},
            "where": {"cmp": ["#spacecraft", "==", "MMS1"]}
        });
        assert_eq!(words(rule), "“All candidates” where spacecraft = MMS1");
        let unknown = json!({"catalogue": "c9"});
        assert_eq!(words(unknown), "catalogue c9");
    }

    #[test]
    fn nested_alternatives_are_bracketed() {
        let rule = json!({
            "filter": {"all": true},
            "where": {"and": [
                {"or": [{"contains": ["tags", "a"]}, {"contains": ["tags", "b"]}]},
                {"not": {"has": "#region"}},
            ]}
        });
        assert_eq!(
            words(rule),
            "events where (tag a or tag b) and not has region"
        );
    }

    #[test]
    fn set_operators_read_with_their_lengths() {
        let rule = json!({"union": [
            {"shift": {"all": true}, "by": 90},
            {"window": [{"catalogue": "c1"}, {"all": true}], "within": 7200},
        ]});
        assert_eq!(
            words(rule),
            "(every event, shifted by 1 min 30 s) or (“All candidates” within 2 h of every event)"
        );
    }

    #[test]
    fn words_durations_and_relative_times_read_plainly() {
        let rule = json!({
            "filter": {"all": true},
            "where": {"and": [
                {"text": "shock"},
                {"cmp": ["duration", ">", 600]},
                {"cmp": ["created", ">", {"now": "-7d"}]},
                {"cmp": ["start", "<", {"now": ""}]},
            ]}
        });
        assert_eq!(
            words(rule),
            "events where mentions “shock” and duration > 10 min and created > now-7d \
             and start < now"
        );
    }

    #[test]
    fn times_and_durations_from_a_search_read_as_dates_and_lengths() {
        let rule = json!({
            "filter": {"all": true},
            "where": {"and": [
                {"cmp": ["start", ">=", 1_420_070_400_000_000_000_i64]},
                {"cmp": ["duration", ">", 600]},
            ]}
        });
        assert_eq!(
            words(rule),
            "events where start ≥ 2015-01-01 00:00:00 and duration > 10 min"
        );
    }

    #[test]
    fn an_invalid_rule_gives_the_core_error() {
        let name = |_: &str| None;
        let rule = json!({"filter": {"all": true}, "where": {"has": "colour"}});
        let error = describe(&rule, &name).unwrap_err();
        assert_eq!(
            error.to_string(),
            "rule.where.has: colour is neither an event field nor an attribute (#key)"
        );
    }
}

/// The line under a page's title: what it is, its size, and how many the filter shows.
/// `picked`: how many of the `total` were picked by hand, when the catalogue has a search.
pub fn counts_text(
    kind: &str,
    unit: &str,
    total: usize,
    picked: Option<usize>,
    visible: usize,
) -> String {
    let plural = if total == 1 { "" } else { "s" };
    let size = match picked {
        None => format!("{kind} · {total} {unit}{plural}"),
        Some(0) => format!("{kind} · {total} {unit}{plural} found by its search"),
        Some(picked) => {
            let found = total.saturating_sub(picked);
            format!("{kind} · {total} {unit}{plural}: {picked} picked, {found} from its search")
        }
    };
    match visible == total {
        true => size,
        false => format!("{size} · {visible} shown"),
    }
}

/// Where a copied catalogue came from: a line naming its direct source, and a hover text
/// listing the whole chain of copies, oldest first. `None` when it was never copied.
pub fn copied_text(chain: &[Hop]) -> Option<(String, String)> {
    let source = chain.last()?;
    let line = format!("Copied from {}", place_text(source));
    let steps: Vec<String> = chain
        .iter()
        .enumerate()
        .map(|(index, hop)| format!("{}. {}", index + 1, step_text(hop)))
        .collect();
    Some((line, format!("Copied, oldest first:\n{}", steps.join("\n"))))
}

/// "<project> › <catalogue>, <date>".
fn place_text(hop: &Hop) -> String {
    let when = Timestamp::from_unix_millis(hop.at)
        .and_then(Timestamp::unix_nanos)
        .map(date_text)
        .unwrap_or_default();
    format!("{} › {}, {when}", hop.project_name, hop.catalogue_name)
}

fn step_text(hop: &Hop) -> String {
    match hop.by.as_deref() {
        Some(by) => format!("{}, by {}", place_text(hop), person_text(by)),
        None => place_text(hop),
    }
}

#[cfg(test)]
mod copied_tests {
    use super::copied_text;
    use cocat_core::model::Hop;

    fn hop(project: &str, catalogue: &str, by: Option<&str>) -> Hop {
        Hop {
            project: format!("{project}-id"),
            project_name: project.to_owned(),
            catalogue: format!("{catalogue}-uuid"),
            catalogue_name: catalogue.to_owned(),
            version: None,
            at: 1_762_771_200_000,
            by: by.map(str::to_owned),
        }
    }

    #[test]
    fn a_catalogue_never_copied_says_nothing() {
        assert_eq!(copied_text(&[]), None);
    }

    #[test]
    fn the_line_names_the_direct_source_and_the_title_the_whole_chain() {
        let chain = [
            hop("Public catalogues", "Bow shock list", None),
            hop("Lab", "Bow shocks", Some("alice.martin@example.org")),
        ];
        let (line, title) = copied_text(&chain).unwrap();
        assert_eq!(line, "Copied from Lab › Bow shocks, 2025-11-10");
        assert_eq!(
            title,
            "Copied, oldest first:\n1. Public catalogues › Bow shock list, 2025-11-10\n2. Lab › Bow shocks, 2025-11-10, by alice.martin"
        );
    }
}

#[cfg(test)]
mod counts_tests {
    use super::counts_text;

    #[test]
    fn a_filter_says_how_many_it_shows() {
        assert_eq!(
            counts_text("Catalogue", "event", 6, None, 2),
            "Catalogue · 6 events · 2 shown"
        );
    }

    #[test]
    fn an_unfiltered_catalogue_gives_its_size_alone() {
        assert_eq!(
            counts_text("Catalogue", "event", 1, None, 1),
            "Catalogue · 1 event"
        );
    }

    #[test]
    fn a_catalogue_with_a_search_says_what_it_picked_and_what_the_search_brought() {
        assert_eq!(
            counts_text("Catalogue", "event", 14, Some(6), 14),
            "Catalogue · 14 events: 6 picked, 8 from its search"
        );
        assert_eq!(
            counts_text("Catalogue", "event", 14, Some(6), 3),
            "Catalogue · 14 events: 6 picked, 8 from its search · 3 shown"
        );
    }

    #[test]
    fn a_catalogue_holding_only_what_its_search_finds_says_so() {
        assert_eq!(
            counts_text("Smart catalogue", "event", 8, Some(0), 8),
            "Smart catalogue · 8 events found by its search"
        );
    }
}
