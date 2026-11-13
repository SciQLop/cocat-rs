//! The rule format of smart catalogues (design section 3): parsing and validation.

use cocat_core::rules::{self, Op, Predicate, Problem, RuleError, Set};
use serde_json::{Value, json};

fn parsed(rule: Value) -> Set {
    rules::parse(&rule).unwrap()
}

fn refused(rule: Value) -> RuleError {
    rules::parse(&rule).unwrap_err()
}

#[test]
fn every_set_parses() {
    let rule = json!({"union": [
        {"all": true},
        {"catalogue": "c"},
        {"filter": {"all": true}, "where": {"has": "#q"}},
        {"merge": {"all": true}, "gap": 60},
        {"merge": {"all": true}},
        {"intersect": [{"all": true}, {"catalogue": "c"}]},
        {"subtract": [{"all": true}, {"catalogue": "c"}]},
        {"shift": {"all": true}, "by": -1.5},
        {"tile": {"all": true}, "width": 3600},
        {"window": [{"all": true}, {"catalogue": "c"}], "within": 10},
        {"closest": [{"all": true}, {"catalogue": "c"}]},
    ]});
    let Set::Union { union } = parsed(rule) else {
        panic!("not a union")
    };
    assert_eq!(union.len(), 11);
    assert_eq!(
        union[4],
        Set::Merge {
            merge: Box::new(Set::All { all: true }),
            gap: 0.0
        }
    );
}

#[test]
fn every_predicate_parses() {
    let rule = json!({"filter": {"all": true}, "where": {"and": [
        {"cmp": ["start", ">=", 5]},
        {"match": ["author", "^a"]},
        {"has": "#q"},
        {"contains": ["tags", "shock"]},
        {"not": {"in_catalogue": "c"}},
        {"or": [{"in_catalogue": null}]},
    ]}});
    let Set::Filter { predicate, .. } = parsed(rule) else {
        panic!("not a filter")
    };
    let Predicate::And(all) = predicate else {
        panic!("not an and")
    };
    assert_eq!(all[0], Predicate::Cmp("start".into(), Op::Ge, json!(5)));
    assert_eq!(all[5], Predicate::Or(vec![Predicate::InCatalogue(None)]));
}

#[test]
fn a_rule_serialises_back_to_its_json() {
    let rule = json!({"window": [
        {"filter": {"all": true}, "where": {"cmp": ["#q", "!=", "x"]}},
        {"catalogue": "c"}
    ], "within": 10.0});
    assert_eq!(serde_json::to_value(parsed(rule.clone())).unwrap(), rule);
}

/// The message of a refused rule: where in the rule, then what is wrong.
fn message(rule: Value) -> String {
    refused(rule).to_string()
}

fn problem(rule: Value) -> Problem {
    match refused(rule) {
        RuleError::Invalid { problem, .. } => problem,
        other => panic!("not a validation error: {other:?}"),
    }
}

#[test]
fn an_unknown_key_is_refused() {
    assert_eq!(
        message(json!({"filter": {"all": true}, "where": {"has": "#q"}, "typo": 1})),
        "rule: unknown key 'typo' for 'filter'"
    );
    assert_eq!(
        message(json!({"all": true, "gap": 1})),
        "rule: unknown key 'gap' for 'all'"
    );
}

#[test]
fn an_unknown_operator_names_its_place() {
    assert_eq!(
        message(json!({"filter": {"all": true}, "where": {"cmp": ["start", "=~", 1]}})),
        "rule.where.cmp: unknown operator '=~'"
    );
}

#[test]
fn a_missing_option_names_its_set() {
    assert_eq!(
        message(json!({"union": [{"all": true}, {"tile": {"all": true}}]})),
        "rule.union[1]: 'tile' needs 'width'"
    );
}

#[test]
fn a_misspelt_set_lists_the_known_ones() {
    assert_eq!(
        message(json!({"subtract": [{"all": true}, {"catalog": "c"}]})),
        "rule.subtract[1]: not a set: expected one of all, public, catalogue, filter, merge, \
         intersect, subtract, union, shift, tile, window, closest"
    );
}

#[test]
fn a_misspelt_predicate_lists_the_known_ones() {
    assert_eq!(
        message(json!({"filter": {"all": true}, "where": {"not": {"equals": ["a", 1]}}})),
        "rule.where.not: unknown predicate 'equals': expected one of cmp, match, has, \
         contains, not, and, or, in_catalogue, text"
    );
}

#[test]
fn a_pair_needs_two_sets() {
    assert_eq!(
        message(json!({"intersect": [{"all": true}]})),
        "rule.intersect: expected 2 items, got 1"
    );
}

#[test]
fn all_must_be_true() {
    assert!(matches!(problem(json!({"all": false})), Problem::All));
}

#[test]
fn an_unknown_field_is_refused() {
    let error = problem(json!({"filter": {"all": true}, "where": {"has": "colour"}}));
    assert!(
        matches!(&error, Problem::Field(field) if field == "colour"),
        "{error:?}"
    );
    let error = problem(json!({"filter": {"all": true}, "where": {"has": "#"}}));
    assert!(
        matches!(&error, Problem::Field(field) if field == "#"),
        "{error:?}"
    );
}

#[test]
fn a_bad_regex_is_refused() {
    let error = problem(json!({"filter": {"all": true}, "where": {"match": ["author", "("]}}));
    assert!(
        matches!(&error, Problem::Regex { pattern, .. } if pattern == "("),
        "{error:?}"
    );
}

#[test]
fn a_negative_or_infinite_length_is_refused() {
    for rule in [
        json!({"merge": {"all": true}, "gap": -1}),
        json!({"window": [{"all": true}, {"all": true}], "within": -1}),
        json!({"tile": {"all": true}, "width": 0}),
        json!({"tile": {"all": true}, "width": -5}),
    ] {
        let error = problem(rule.clone());
        assert!(matches!(error, Problem::Length(_)), "{rule}: {error:?}");
    }
    assert_eq!(
        message(json!({"tile": {"all": true}, "width": 0})),
        "rule.width: 0 is not a length in seconds this rule takes"
    );
}

#[test]
fn a_negative_shift_is_valid() {
    parsed(json!({"shift": {"all": true}, "by": -3600}));
}

#[test]
fn validation_reaches_nested_sets() {
    let rule = json!({"union": [{"all": true}, {"subtract": [
        {"all": true},
        {"filter": {"all": true}, "where": {"not": {"match": ["#x", "["]}}}
    ]}]});
    assert_eq!(
        message(rule.clone()),
        "rule.union[1].subtract[1].where.not.match: [ is not a regular expression"
    );
    assert!(matches!(problem(rule), Problem::Regex { .. }));
}

#[test]
fn a_rule_says_whether_it_takes_events_from_the_public_catalogues() {
    let reaches = |rule: Value| rules::parse(&rule).unwrap().reaches_public();
    assert!(reaches(
        json!({"filter": {"public": true}, "where": {"text": "x"}})
    ));
    assert!(reaches(
        json!({"union": [{"all": true}, {"merge": {"public": true}, "gap": 60}]})
    ));
    assert!(!reaches(
        json!({"filter": {"all": true}, "where": {"text": "x"}})
    ));
}
