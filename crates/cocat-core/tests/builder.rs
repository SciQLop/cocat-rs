//! The search builder's vocabulary: fields, tests, the text a row writes, and the rows a
//! flat search reads back into. Both apps (the web one and SciQLop) speak it.

use cocat_core::query::builder::{self, Row, Test};
use cocat_core::query::{self, Kind};

const SHOCKS: &str = "5a5a5a5a-0000-4000-8000-00000000000a";

fn catalogues() -> Vec<(String, String)> {
    vec![(SHOCKS.to_owned(), "Bow shocks".to_owned())]
}

fn row(field: &str, test: Test, value: &str) -> Row {
    Row {
        field: field.to_owned(),
        test,
        value: value.to_owned(),
    }
}

/// The rows a search reads back into, through its rule.
fn rows_of(text: &str) -> Option<(bool, Vec<Row>)> {
    let predicate = query::parse(text, &catalogues()).unwrap();
    builder::rows(&predicate, &catalogues())
}

#[test]
fn every_field_has_a_label_and_a_kind_name() {
    let labels: Vec<String> = query::FIELDS
        .iter()
        .map(|field| builder::field_label(field.name))
        .collect();
    assert_eq!(labels[..3], ["Start", "Stop", "Duration"]);
    assert_eq!(builder::field_label("created"), "Added");
    assert_eq!(builder::field_label("origin"), "Made by");
    assert_eq!(builder::field_label("#density"), "density (attribute)");
    for field in query::FIELDS {
        assert_eq!(Kind::named(field.kind.name()), Some(field.kind));
    }
    assert_eq!(Kind::named("nonsense"), None);
}

#[test]
fn each_kind_offers_its_tests_with_the_web_words() {
    let words = |kind| -> Vec<&str> {
        builder::tests(kind)
            .iter()
            .map(|&test| builder::label(kind, test))
            .collect()
    };
    assert_eq!(
        words(Kind::Time),
        [
            "is in the last … days",
            "is on",
            "is after",
            "is before",
            "is set",
            "is not set"
        ]
    );
    assert_eq!(words(Kind::Tags), ["has", "does not have"]);
    assert_eq!(words(Kind::Catalogue), ["is in", "is not in"]);
    assert_eq!(words(Kind::Duration), ["longer than", "shorter than"]);
    assert_eq!(words(Kind::Presence), ["has one", "has none"]);
    for test in [Test::Set, Test::NotSet] {
        assert!(!test.needs_value());
    }
    assert!(Test::Contains.needs_value());
    for kind in [
        Kind::Text,
        Kind::Number,
        Kind::Attribute,
        Kind::Time,
        Kind::Duration,
        Kind::Tags,
    ] {
        for &test in builder::tests(kind) {
            assert_eq!(Test::named(test.name()), Some(test));
        }
    }
}

#[test]
fn rows_write_the_web_text() {
    assert_eq!(
        builder::clause("confidence", Test::AtLeast, "0.8"),
        "confidence:>=0.8"
    );
    assert_eq!(
        builder::clause("tag", Test::IsNot, "shock"),
        "NOT tag:shock"
    );
    assert_eq!(
        builder::clause("created", Test::InLastDays, "7"),
        "created:now-7d"
    );
    assert_eq!(builder::clause("status", Test::Set, ""), "_exists_:status");
    assert_eq!(
        builder::clause("catalogue", Test::Is, "Bow shocks"),
        "catalogue:\"Bow shocks\""
    );
    let rows = ["tag:shock".to_owned(), "NOT status:final".to_owned()];
    assert_eq!(
        builder::search_text(&rows, false),
        "tag:shock NOT status:final"
    );
    assert_eq!(
        builder::search_text(&rows, true),
        "(tag:shock) OR (NOT status:final)"
    );
}

#[test]
fn text_that_is_a_value_finds_only_that_value() {
    let is = query::parse(&builder::clause("author", Test::Is, "al"), &[]).unwrap();
    let contains = query::parse(&builder::clause("author", Test::Contains, "al"), &[]).unwrap();
    assert_ne!(is, contains, "“is” on text must not mean “contains”");
    let attribute = query::parse(&builder::clause("#mode", Test::Is, "burst"), &[]).unwrap();
    let contained = query::parse(&builder::clause("#mode", Test::Contains, "burst"), &[]).unwrap();
    assert_ne!(attribute, contained);
}

fn samples() -> Vec<(&'static str, Kind, &'static str)> {
    vec![
        ("author", Kind::Text, "alice smith"),
        ("confidence", Kind::Number, "0.8"),
        ("#density", Kind::Attribute, "12"),
        ("#mode", Kind::Attribute, "burst"),
        ("created", Kind::Time, "2015-03-01"),
        ("start", Kind::Time, "2015-03-01T12:30:00Z"),
        ("duration", Kind::Duration, "10min"),
        ("status", Kind::Enum(&query::STATUSES), "reviewed"),
        ("tag", Kind::Tags, "shock"),
        ("product", Kind::Tags, "amda/b_gse"),
        ("catalogue", Kind::Catalogue, "Bow shocks"),
        ("shape", Kind::Presence, ""),
    ]
}

/// A row with a value its test takes.
fn sample_row(field: &str, test: Test, value: &str) -> Row {
    let value = match test {
        Test::InLastDays => "7",
        Test::Matches => "^al",
        Test::On => "2015-03-01",
        _ if !test.needs_value() => "",
        _ => value,
    };
    row(field, test, value)
}

#[test]
fn every_test_of_every_kind_reads_back_as_its_row() {
    for (field, kind, value) in samples() {
        for &test in builder::tests(kind) {
            let made = sample_row(field, test, value);
            let text = builder::clause(&made.field, made.test, &made.value);
            assert_eq!(
                rows_of(&text),
                Some((false, vec![made.clone()])),
                "{field} {test:?}: {text}"
            );
        }
    }
}

#[test]
fn all_and_any_read_back_with_their_joint() {
    let made = vec![
        row("tag", Test::Is, "shock"),
        row("confidence", Test::AtLeast, "0.7"),
        row("created", Test::On, "2015-03-01"),
        row("status", Test::IsNot, "final"),
    ];
    let clauses: Vec<String> = made
        .iter()
        .map(|made| builder::clause(&made.field, made.test, &made.value))
        .collect();
    for any in [false, true] {
        let text = builder::search_text(&clauses, any);
        assert_eq!(rows_of(&text), Some((any, made.clone())), "{text}");
    }
    assert_eq!(rows_of(""), Some((false, Vec::new())));
    assert_eq!(
        rows_of("confidence:[0.2 TO 0.5]"),
        Some((
            false,
            vec![
                row("confidence", Test::AtLeast, "0.2"),
                row("confidence", Test::AtMost, "0.5")
            ]
        )),
        "a range is its two bounds"
    );
}

#[test]
fn rich_searches_have_no_rows() {
    for text in [
        "shock",
        "tag:a (tag:b OR tag:c)",
        "NOT (tag:a tag:b)",
        "NOT confidence:>=0.5",
        "confidence:<=0.5 OR (tag:a tag:b)",
        "_exists_:catalogue",
    ] {
        assert_eq!(rows_of(text), None, "{text}");
    }
}

#[test]
fn a_rule_reads_back_into_rows_with_its_catalogues_named() {
    let rule = serde_json::json!({
        "filter": {"all": true},
        "where": {"in_catalogue": SHOCKS},
    });
    assert_eq!(
        builder::rule_rows(&rule, &catalogues()),
        Some((false, vec![row("catalogue", Test::Is, "Bow shocks")]))
    );
    let merged = serde_json::json!({"merge": {"all": true}, "gap": 60});
    assert_eq!(builder::rule_rows(&merged, &catalogues()), None);
}

#[test]
fn an_error_says_where_how_long_and_what_was_meant() {
    let error = query::parse("tag:shock auhtor:bob", &[]).unwrap_err();
    assert_eq!(error.position, 10);
    assert_eq!(error.length, Some(6));
    assert_eq!(error.suggestion.as_deref(), Some("author"));
    assert!(error.message.starts_with("there is no field “auhtor”"));
    let error = query::parse("tag:shock OR", &[]).unwrap_err();
    assert_eq!(error.suggestion, None);
    let error = query::parse("confidence:>=high", &[]).unwrap_err();
    assert_eq!((error.position, error.length), (13, Some(4)));
}
