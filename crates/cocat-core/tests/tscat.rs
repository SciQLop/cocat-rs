//! tscat predicates (`tscat/filtering.py`, `to_dict`) become rules (design section 3).

use cocat_core::rules::{self, Problem, RuleError, TscatError, rule_from_tscat};
use serde_json::{Value, json};

const CATALOGUE: &str = "957d65ae-f278-48f5-aab1-8cf50efeadef";

fn field(name: &str) -> Value {
    json!({"type": "Field", "name": name})
}

fn attribute(name: &str) -> Value {
    json!({"type": "Attribute", "name": name})
}

fn comparison(op: &str, lhs: Value, rhs: Value) -> Value {
    json!({"type": "Comparison", "op": op, "lhs": lhs, "rhs": rhs})
}

/// The `where` of the rule a tscat predicate becomes, after checking the rule's shape.
fn converted(predicate: Value) -> Value {
    let rule = rule_from_tscat(&predicate).unwrap();
    rules::parse(&rule).unwrap();
    assert_eq!(rule["filter"], json!({"all": true}));
    rule["where"].clone()
}

#[test]
fn a_field_comparison_keeps_its_field() {
    let predicate = comparison("==", field("author"), json!("Patrick"));
    assert_eq!(
        converted(predicate),
        json!({"cmp": ["author", "==", "Patrick"]})
    );
}

#[test]
fn an_attribute_comparison_names_the_attribute_key() {
    let predicate = comparison(">=", attribute("b"), json!(11));
    assert_eq!(converted(predicate), json!({"cmp": ["#b", ">=", 11]}));
}

#[test]
fn a_rating_comparison_becomes_a_confidence_comparison() {
    let predicate = comparison(">", field("rating"), json!(7));
    assert_eq!(
        converted(predicate),
        json!({"cmp": ["confidence", ">", 0.7]})
    );
}

#[test]
fn a_datetime_becomes_nanoseconds() {
    let naive = json!({"type": "Comparison", "op": "<", "lhs": field("start"),
        "rhs": "2020-01-01T00:00:00", "rhs_type": "datetime"});
    assert_eq!(
        converted(naive),
        json!({"cmp": ["start", "<", 1_577_836_800_000_000_000_i64]})
    );
    let aware = json!({"type": "Comparison", "op": ">=", "lhs": field("stop"),
        "rhs": "2020-01-01T02:00:00.5+02:00", "rhs_type": "datetime"});
    assert_eq!(
        converted(aware),
        json!({"cmp": ["stop", ">=", 1_577_836_800_500_000_000_i64]})
    );
}

#[test]
fn a_datetime_that_does_not_read_is_refused() {
    let predicate = json!({"type": "Comparison", "op": "<", "lhs": field("start"),
        "rhs": "yesterday", "rhs_type": "datetime"});
    let error = rule_from_tscat(&predicate).unwrap_err();
    assert!(matches!(error, TscatError::Datetime(_)), "{error:?}");
}

#[test]
fn match_has_in_and_uuid_convert() {
    let predicate = json!({"type": "Match", "lhs": attribute("s"), "rhs": "^G"});
    assert_eq!(converted(predicate), json!({"match": ["#s", "^G"]}));

    let predicate = json!({"type": "Has", "operand": attribute("f")});
    assert_eq!(converted(predicate), json!({"has": "#f"}));

    let predicate = json!({"type": "In", "lhs": "shock", "rhs": field("tags")});
    assert_eq!(converted(predicate), json!({"contains": ["tags", "shock"]}));

    let predicate = json!({"type": "In", "lhs": "a", "rhs": attribute("list")});
    assert_eq!(converted(predicate), json!({"contains": ["#list", "a"]}));

    let predicate = json!({"type": "UUID", "uuid": CATALOGUE});
    assert_eq!(
        converted(predicate),
        json!({"cmp": ["uuid", "==", CATALOGUE]})
    );
}

#[test]
fn in_catalogue_converts_with_or_without_a_catalogue() {
    let predicate = json!({"type": "InCatalogue", "catalogue_uuid": CATALOGUE});
    assert_eq!(converted(predicate), json!({"in_catalogue": CATALOGUE}));
    let predicate = json!({"type": "InCatalogue", "catalogue_uuid": null});
    assert_eq!(converted(predicate), json!({"in_catalogue": null}));
}

#[test]
fn a_nested_all_any_not_tree_converts_whole() {
    let predicate = json!({"type": "All", "predicates": [
        {"type": "Any", "predicates": [
            comparison("==", field("author"), json!("Patrick")),
            {"type": "Not", "operand": {"type": "Has", "operand": attribute("g")}},
        ]},
        {"type": "Not", "operand": {"type": "All", "predicates": [
            {"type": "Match", "lhs": field("author"), "rhs": "a"},
            comparison("<=", field("rating"), json!(5)),
        ]}},
    ]});
    assert_eq!(
        converted(predicate),
        json!({"and": [
            {"or": [
                {"cmp": ["author", "==", "Patrick"]},
                {"not": {"has": "#g"}},
            ]},
            {"not": {"and": [
                {"match": ["author", "a"]},
                {"cmp": ["confidence", "<=", 0.5]},
            ]}},
        ]})
    );
}

#[test]
fn an_unknown_node_is_refused() {
    let error = rule_from_tscat(&json!({"type": "Xor", "predicates": []})).unwrap_err();
    assert!(matches!(error, TscatError::Format(_)), "{error:?}");
}

#[test]
fn a_catalogue_field_is_not_an_event_field() {
    let error = rule_from_tscat(&comparison("==", field("name"), json!("x"))).unwrap_err();
    assert!(
        matches!(&error, TscatError::Rule(RuleError::Invalid { problem: Problem::Field(name), .. }) if name == "name"),
        "{error:?}"
    );
}

#[test]
fn a_bad_regex_is_refused() {
    let predicate = json!({"type": "Match", "lhs": field("author"), "rhs": "("});
    let error = rule_from_tscat(&predicate).unwrap_err();
    assert!(
        matches!(
            error,
            TscatError::Rule(RuleError::Invalid {
                problem: Problem::Regex { .. },
                ..
            })
        ),
        "{error:?}"
    );
}
