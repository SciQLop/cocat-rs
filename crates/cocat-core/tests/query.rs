//! The search language (design: "The query language"): reading, printing, errors.

use cocat_core::query::{self, FIELDS, Kind, QueryError};
use cocat_core::rules::{Op, Predicate};
use cocat_core::time::Timestamp;
use serde_json::{Value, json};

const SHOCKS: &str = "5a5a5a5a-0000-4000-8000-00000000000a";
const GONE: &str = "5a5a5a5a-0000-4000-8000-00000000000b";

fn catalogues() -> Vec<(String, String)> {
    vec![
        (SHOCKS.to_owned(), "Bow shocks".to_owned()),
        ("c2".to_owned(), "twin".to_owned()),
        ("c3".to_owned(), "twin".to_owned()),
    ]
}

fn read(text: &str) -> Predicate {
    query::parse(text, &catalogues()).unwrap_or_else(|error| panic!("{text}: {error}"))
}

fn refused(text: &str) -> QueryError {
    query::parse(text, &catalogues()).unwrap_err()
}

fn printed(predicate: &Predicate) -> Option<String> {
    query::print(predicate, &catalogues())
}

fn nanos(text: &str) -> Value {
    Timestamp::parse(text).unwrap().unix_nanos().unwrap().into()
}

fn cmp(field: &str, op: Op, value: Value) -> Predicate {
    Predicate::Cmp(field.to_owned(), op, value)
}

fn text(word: &str) -> Predicate {
    Predicate::Text(word.to_owned())
}

fn and(clauses: Vec<Predicate>) -> Predicate {
    Predicate::And(clauses)
}

fn not(inner: Predicate) -> Predicate {
    Predicate::Not(Box::new(inner))
}

fn matches(field: &str, pattern: &str) -> Predicate {
    Predicate::Match(field.to_owned(), pattern.to_owned())
}

#[test]
fn words_and_phrases_match_any_text() {
    assert_eq!(read("shock"), text("shock"));
    assert_eq!(read("\"bow wave\""), text("bow wave"));
    assert_eq!(read("  a   b "), and(vec![text("a"), text("b")]));
    assert_eq!(read("14:02"), text("14:02"));
    assert_eq!(read(r#""say \"hi\"""#), text("say \"hi\""));
}

#[test]
fn an_empty_query_matches_everything() {
    assert_eq!(read(""), and(vec![]));
    assert_eq!(read("   "), and(vec![]));
}

#[test]
fn text_fields_contain_ignoring_case_or_match_a_regex() {
    assert_eq!(read("author:alice"), matches("author", "(?i)alice"));
    assert_eq!(
        read("source_id:\"a.b c\""),
        matches("source_id", r"(?i)a\.b c")
    );
    assert_eq!(read("author:/^al/"), matches("author", "^al"));
    assert_eq!(read(r"origin:/a\/b/"), matches("origin", "a/b"));
    assert_eq!(read("author:=bob"), cmp("author", Op::Eq, json!("bob")));
}

#[test]
fn status_is_one_of_its_values() {
    assert_eq!(
        read("status:Reviewed"),
        cmp("status", Op::Eq, json!("reviewed"))
    );
    let error = refused("status:nope");
    assert_eq!(error.position, 7);
    assert_eq!(
        error.message,
        "status is one of preliminary, confirmed, reviewed, final, rejected"
    );
}

#[test]
fn tags_and_products_match_one_word_exactly() {
    let tag = Predicate::Contains("tags".to_owned(), json!("shock"));
    assert_eq!(read("tag:shock"), tag);
    assert_eq!(read("tags:shock"), tag);
    assert_eq!(
        read("product:\"amda/x\""),
        Predicate::Contains("products".to_owned(), json!("amda/x"))
    );
    assert_eq!(
        refused("tag:>a").message,
        "tag takes one word, matched exactly"
    );
}

#[test]
fn numbers_compare_and_take_ranges() {
    assert_eq!(
        read("confidence:>=0.8"),
        cmp("confidence", Op::Ge, json!(0.8))
    );
    assert_eq!(read("confidence:1"), cmp("confidence", Op::Eq, json!(1)));
    assert_eq!(
        read("confidence:[0.2 TO 0.5]"),
        and(vec![
            cmp("confidence", Op::Ge, json!(0.2)),
            cmp("confidence", Op::Le, json!(0.5)),
        ])
    );
    assert_eq!(
        read("start_err:[* TO 3]"),
        cmp("start_err", Op::Le, json!(3))
    );
    assert_eq!(
        read("stop_err:[* TO *]"),
        Predicate::Has("stop_err".to_owned())
    );
    let error = refused("confidence:>high");
    assert_eq!(
        (error.position, error.message.as_str()),
        (12, "expected a number, not “high”")
    );
}

#[test]
fn durations_take_units() {
    assert_eq!(read("duration:>10min"), cmp("duration", Op::Gt, json!(600)));
    assert_eq!(read("duration:90"), cmp("duration", Op::Eq, json!(90)));
    assert_eq!(read("duration:<1.5h"), cmp("duration", Op::Lt, json!(5400)));
    assert_eq!(
        read("duration:[1h TO 2d]"),
        and(vec![
            cmp("duration", Op::Ge, json!(3600)),
            cmp("duration", Op::Le, json!(172_800)),
        ])
    );
    assert!(
        refused("duration:>10w")
            .message
            .starts_with("expected a duration")
    );
}

#[test]
fn a_year_month_or_day_is_the_span_it_covers() {
    let during = |low: &str, high: &str| {
        and(vec![
            cmp("start", Op::Ge, nanos(low)),
            cmp("start", Op::Lt, nanos(high)),
        ])
    };
    assert_eq!(read("start:2015"), during("2015-01-01", "2016-01-01"));
    assert_eq!(read("start:2015-12"), during("2015-12-01", "2016-01-01"));
    assert_eq!(read("start:2016-02-29"), during("2016-02-29", "2016-03-01"));
    assert_eq!(
        read("start:>2015"),
        cmp("start", Op::Ge, nanos("2016-01-01"))
    );
    assert_eq!(
        read("start:>=2015"),
        cmp("start", Op::Ge, nanos("2015-01-01"))
    );
    assert_eq!(
        read("stop:<2015-03"),
        cmp("stop", Op::Lt, nanos("2015-03-01"))
    );
    assert_eq!(
        read("stop:<=2015-03"),
        cmp("stop", Op::Lt, nanos("2015-04-01"))
    );
    assert_eq!(
        read("start:[2015 TO 2016-06]"),
        during("2015-01-01", "2016-07-01")
    );
}

#[test]
fn a_full_time_is_an_instant() {
    let noon = nanos("2015-03-01T12:00:00Z");
    assert_eq!(
        read("start:>2015-03-01T12:00:00Z"),
        cmp("start", Op::Gt, noon.clone())
    );
    assert_eq!(
        read("start:2015-03-01T12:00:00"),
        cmp("start", Op::Eq, noon)
    );
    let error = refused("created:2015-13");
    assert_eq!(error.position, 8);
    assert!(
        error.message.starts_with("“2015-13” is not a time"),
        "{}",
        error.message
    );
}

#[test]
fn now_is_relative() {
    assert_eq!(
        read("created:>now-7d"),
        cmp("created", Op::Gt, json!({"now": "-7d"}))
    );
    assert_eq!(read("stop:<now"), cmp("stop", Op::Lt, json!({"now": ""})));
    assert!(refused("created:>now-7w").message.contains("is not a time"));
}

#[test]
fn a_bare_relative_time_means_since_then() {
    assert_eq!(
        read("created:now-7d"),
        cmp("created", Op::Ge, json!({"now": "-7d"}))
    );
}

#[test]
fn existence_and_catalogues() {
    assert_eq!(read("_exists_:shape"), Predicate::Has("shape".to_owned()));
    assert_eq!(
        read("_exists_:#density"),
        Predicate::Has("#density".to_owned())
    );
    assert_eq!(
        read("NOT(_exists_:tag)"),
        not(Predicate::Has("tags".to_owned()))
    );
    assert_eq!(read("NOT _exists_:catalogue"), Predicate::InCatalogue(None));
    assert_eq!(
        read("_exists_:catalogue"),
        not(Predicate::InCatalogue(None))
    );
    assert_eq!(
        read("catalogue:\"Bow shocks\""),
        Predicate::InCatalogue(Some(SHOCKS.to_owned()))
    );
    assert_eq!(
        read(&format!("catalogue:{GONE}")),
        Predicate::InCatalogue(Some(GONE.to_owned()))
    );
    assert_eq!(
        refused("catalogue:nope").message,
        "no catalogue is named “nope”"
    );
    assert_eq!(
        refused("catalogue:twin").message,
        "several catalogues are named “twin”: use its uuid"
    );
    assert_eq!(
        refused("shape:x").message,
        "shape is only searched for with _exists_:shape"
    );
}

#[test]
fn attributes_are_numbers_when_they_read_as_one() {
    assert_eq!(read("#density:>10"), cmp("#density", Op::Gt, json!(10)));
    assert_eq!(read("#n:3"), cmp("#n", Op::Eq, json!(3)));
    assert_eq!(read("#n:\"3\""), matches("#n", "(?i)3"));
    assert_eq!(read("#label:bow"), matches("#label", "(?i)bow"));
    assert_eq!(read("#label:=Bow"), cmp("#label", Op::Eq, json!("Bow")));
    assert_eq!(read("#label:>m"), cmp("#label", Op::Gt, json!("m")));
}

#[test]
fn not_binds_tighter_than_and_than_or() {
    assert_eq!(
        read("a OR b c"),
        Predicate::Or(vec![text("a"), and(vec![text("b"), text("c")])])
    );
    assert_eq!(read("NOT a b"), and(vec![not(text("a")), text("b")]));
    assert_eq!(read("a AND b"), read("a b"));
    assert_eq!(
        read("(a OR b) c"),
        and(vec![Predicate::Or(vec![text("a"), text("b")]), text("c")])
    );
    assert_eq!(
        read("a (b c) d"),
        and(vec![text("a"), text("b"), text("c"), text("d")])
    );
    assert_eq!(read("NOT NOT a"), text("a"));
    assert_eq!(read("ORANGE NOTE"), and(vec![text("ORANGE"), text("NOTE")]));
}

#[test]
fn an_unknown_field_names_the_closest_one() {
    let error = refused("tag:x auhtor:alice");
    assert_eq!(error.position, 6);
    assert_eq!(
        error.message,
        "there is no field “auhtor”: did you mean “author”?"
    );
    assert_eq!(
        error.to_string(),
        "there is no field “auhtor”: did you mean “author”? (at character 7)"
    );
}

#[test]
fn syntax_errors_say_where() {
    let at = |text: &str| {
        let error = refused(text);
        (error.position, error.message)
    };
    assert_eq!(at("(a b"), (4, "expected a closing )".to_owned()));
    assert_eq!(at("tag:"), (4, "expected a value".to_owned()));
    assert_eq!(
        at("a OR"),
        (4, "expected a search term after OR".to_owned())
    );
    assert_eq!(at("\"open"), (5, "expected a closing \"".to_owned()));
    assert_eq!(at("été )"), (4, "unexpected “)”".to_owned()));
    assert_eq!(at("confidence:[1 2]"), (14, "expected TO".to_owned()));
}

#[test]
fn printing_gives_canonical_text() {
    let cases = [
        ("  shock   tag:a ", "shock tag:a"),
        ("a AND (b OR c)", "a (b OR c)"),
        ("a OR (b c)", "a OR b c"),
        ("NOT (a b)", "NOT (a b)"),
        ("NOT(_exists_:shape)", "NOT _exists_:shape"),
        ("author:Alice", "author:Alice"),
        ("author:\"a b\"", "author:\"a b\""),
        ("author:=bob", "author:=bob"),
        ("author:/^a\\/b$/", "author:/^a\\/b$/"),
        ("status:REVIEWED", "status:reviewed"),
        ("tags:x", "tag:x"),
        (
            "confidence:[0.2 TO 0.5]",
            "confidence:>=0.2 confidence:<=0.5",
        ),
        ("duration:>3600", "duration:>1h"),
        ("duration:<90", "duration:<90"),
        ("start:2015", "start:2015"),
        ("start:2015-03 tag:x", "start:2015-03 tag:x"),
        ("stop:2015-03-01", "stop:2015-03-01"),
        ("start:>2015", "start:>=2016-01-01"),
        (
            "start:[2015 TO 2016]",
            "start:>=2015-01-01 start:<2017-01-01",
        ),
        ("start:>2015-03-01T12:00:00", "start:>2015-03-01T12:00:00Z"),
        ("created:>now-7d", "created:>now-7d"),
        ("catalogue:\"Bow shocks\"", "catalogue:\"Bow shocks\""),
        ("NOT _exists_:catalogue", "NOT _exists_:catalogue"),
        ("_exists_:catalogue", "_exists_:catalogue"),
        ("#density:>10 #label:x", "#density:>10 #label:x"),
        ("#n:\"3\"", "#n:\"3\""),
        ("\"14:02\" \"NOT\" \"a b\"", "\"14:02\" \"NOT\" \"a b\""),
        ("", ""),
    ];
    for (query, expected) in cases {
        assert_eq!(printed(&read(query)).as_deref(), Some(expected), "{query}");
    }
}

#[test]
fn printed_text_reads_back_the_same() {
    let queries = [
        "a OR NOT b c OR (d e)",
        "tag:\"with space\" product:x/y",
        "start:[* TO 2015-06] duration:[10min TO *]",
        "created:<now+1y stop:>=now-90s",
        "#a:1.5 #b:\"x y\" #c:/z+/",
        "NOT (status:final OR status:rejected) confidence:<0.5",
    ];
    for query in queries {
        let predicate = read(query);
        let text = printed(&predicate).unwrap_or_else(|| panic!("{query} has no text"));
        assert_eq!(read(&text), predicate, "{query} printed as {text}");
    }
}

#[test]
fn a_predicate_the_language_cannot_say_has_no_text() {
    for predicate in [
        cmp("author", Op::Ne, json!("bob")),
        matches("tags", "x"),
        cmp("status", Op::Lt, json!("final")),
        Predicate::Contains("author".to_owned(), json!("x")),
        Predicate::InCatalogue(Some("not a uuid".to_owned())),
    ] {
        assert_eq!(printed(&predicate), None, "{predicate:?}");
    }
}

#[test]
fn a_hand_written_predicate_prints_in_its_canonical_form() {
    let hand = and(vec![
        and(vec![cmp("confidence", Op::Gt, json!(0.5))]),
        cmp("duration", Op::Ge, json!(600.0)),
        cmp("author", Op::Eq, Value::Null),
        not(not(text("x"))),
    ]);
    assert_eq!(
        printed(&hand).as_deref(),
        Some("confidence:>0.5 duration:>=10min NOT _exists_:author x")
    );
}

#[test]
fn a_filter_rule_has_a_query_and_a_source() {
    let all = json!({"filter": {"all": true}, "where": {"contains": ["tags", "x"]}});
    assert_eq!(
        query::rule_query(&all, &catalogues()),
        Some(("tag:x".to_owned(), None))
    );
    let within = json!({"filter": {"catalogue": SHOCKS}, "where": {"text": "y"}});
    assert_eq!(
        query::rule_query(&within, &catalogues()),
        Some(("y".to_owned(), Some(SHOCKS.to_owned())))
    );
    let merged = json!({"merge": {"all": true}, "gap": 1});
    assert_eq!(query::rule_query(&merged, &catalogues()), None);
    assert_eq!(query::rule(&read("tag:x"), None), all);
    assert_eq!(query::rule(&text("y"), Some(SHOCKS)), within);
}

#[test]
fn every_field_has_a_kind_and_attributes_are_attributes() {
    assert_eq!(query::kind_of("tags"), Some(Kind::Tags));
    assert_eq!(query::kind_of("#anything"), Some(Kind::Attribute));
    assert_eq!(query::kind_of("colour"), None);
    let Some(status) = FIELDS.iter().find(|field| field.name == "status") else {
        panic!("no status field")
    };
    let Kind::Enum(values) = status.kind else {
        panic!("status is not an enum")
    };
    for value in values {
        assert!(
            cocat_core::model::Status::from_name(value).is_some(),
            "{value}"
        );
    }
    for field in FIELDS {
        let rule = json!({"filter": {"all": true}, "where": {"has": field.rule}});
        if field.kind != Kind::Catalogue {
            cocat_core::rules::parse(&rule).unwrap_or_else(|error| panic!("{error}"));
        }
    }
}

#[test]
fn a_search_rule_reads_as_one_search_of_all_events() {
    let all = json!({"filter": {"all": true}, "where": {"contains": ["tags", "x"]}});
    assert_eq!(
        query::rule_search(&all, &catalogues()),
        Some("tag:x".to_owned())
    );
    let within =
        json!({"filter": {"catalogue": SHOCKS}, "where": {"or": [{"text": "y"}, {"text": "z"}]}});
    let text = query::rule_search(&within, &catalogues());
    assert_eq!(text.as_deref(), Some(r#"catalogue:"Bow shocks" (y OR z)"#));
    let merged = json!({"merge": {"all": true}, "gap": 1});
    assert_eq!(query::rule_search(&merged, &catalogues()), None);
}

#[test]
fn a_search_rule_with_nothing_but_its_source_reads_as_that_catalogue() {
    let within = json!({"filter": {"catalogue": SHOCKS}, "where": {"and": []}});
    assert_eq!(
        query::rule_search(&within, &catalogues()).as_deref(),
        Some(r#"catalogue:"Bow shocks""#)
    );
}

#[test]
fn a_search_of_the_public_catalogues_reads_back_and_no_other_rule_does() {
    let public = [("public.donki.cme".to_owned(), "DONKI CMEs".to_owned())];
    let predicate = query::parse("tag:shock catalogue:\"DONKI CMEs\"", &public).unwrap();
    let rule = query::public_rule(&predicate);
    assert_eq!(rule["filter"], json!({"public": true}));
    assert_eq!(
        query::public_search(&rule, &public).as_deref(),
        Some("tag:shock catalogue:\"DONKI CMEs\"")
    );
    assert_eq!(
        query::rule_search(&rule, &public),
        None,
        "not a search of the project"
    );
    let project = query::rule(&predicate, None);
    assert_eq!(query::public_search(&project, &public), None);
    let refused =
        cocat_core::rules::parse(&json!({"filter": {"public": false}, "where": {"text": "x"}}));
    assert!(
        refused
            .unwrap_err()
            .to_string()
            .contains("\"public\" must be true")
    );
}
