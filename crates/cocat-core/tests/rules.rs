//! The rule format of smart catalogues (design section 3): parsing and validation.

use cocat_core::rules::{self, Op, Predicate, RuleError, Set};
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

#[test]
fn an_unknown_key_is_refused() {
    let error = refused(json!({"filter": {"all": true}, "where": {"has": "#q"}, "typo": 1}));
    assert!(matches!(error, RuleError::Format(_)), "{error:?}");
    let error = refused(json!({"all": true, "gap": 1}));
    assert!(matches!(error, RuleError::Format(_)), "{error:?}");
}

#[test]
fn an_unknown_operator_is_refused() {
    let error = refused(json!({"filter": {"all": true}, "where": {"cmp": ["start", "~", 1]}}));
    assert!(matches!(error, RuleError::Format(_)), "{error:?}");
}

#[test]
fn all_must_be_true() {
    assert!(matches!(refused(json!({"all": false})), RuleError::All));
}

#[test]
fn an_unknown_field_is_refused() {
    let error = refused(json!({"filter": {"all": true}, "where": {"has": "colour"}}));
    assert!(
        matches!(&error, RuleError::Field(field) if field == "colour"),
        "{error:?}"
    );
    let error = refused(json!({"filter": {"all": true}, "where": {"has": "#"}}));
    assert!(
        matches!(&error, RuleError::Field(field) if field == "#"),
        "{error:?}"
    );
}

#[test]
fn a_bad_regex_is_refused() {
    let error = refused(json!({"filter": {"all": true}, "where": {"match": ["author", "("]}}));
    assert!(
        matches!(&error, RuleError::Regex { pattern, .. } if pattern == "("),
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
        let error = refused(rule.clone());
        assert!(
            matches!(error, RuleError::Length { .. }),
            "{rule}: {error:?}"
        );
    }
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
    assert!(matches!(refused(rule), RuleError::Regex { .. }));
}
