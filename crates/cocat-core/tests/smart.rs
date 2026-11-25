//! Smart catalogues: a catalogue's members are the events it lists plus what its rule yields
//! (design section 3).

use cocat_core::model::{
    self, EventRecord, Hop, Member, NewCatalogue, OriginKind, Status, WriteError,
};
use cocat_core::rules::{Problem, RuleError};
use cocat_core::time::Timestamp;
use serde_json::{Map, Value, json};
use yrs::{Any, Doc, Map as _, Out, ReadTxn as _, Transact};

const SMART: &str = "5a5a5a5a-0000-4000-8000-000000000001";
const OTHER: &str = "5a5a5a5a-0000-4000-8000-000000000002";
const THIRD: &str = "5a5a5a5a-0000-4000-8000-000000000003";
const BASE: i64 = 1_738_317_600_000_000_000; // 2025-01-31 10:00:00 UTC
fn hop() -> Hop {
    Hop {
        project: "lab".to_owned(),
        project_name: "Lab".to_owned(),
        catalogue: SMART.to_owned(),
        catalogue_name: "Smart".to_owned(),
        version: None,
        at: 1_700_000_000_000,
        by: Some("carol@example.org".to_owned()),
    }
}

const SECOND: i64 = 1_000_000_000;

fn at(offset: i64) -> Timestamp {
    Timestamp::from_unix_nanos(BASE + offset * SECOND).unwrap()
}

fn uuid(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

/// Events 1..=4, a second each, ten seconds apart; 3 is rejected, 4 is in the trash.
fn room() -> Doc {
    let doc = Doc::new();
    let mut txn = doc.transact_mut();
    for n in 1..=4u32 {
        let offset = i64::from(n) * 10;
        let mut event = EventRecord::new(uuid(n), at(offset), at(offset + 1));
        event.author = Some(if n % 2 == 0 { "bob" } else { "alice" }.to_owned());
        event.tags = vec![format!("t{n}")];
        event.attributes = object(json!({"q": n, "label": format!("ev-{n}")}));
        model::create_event(&mut txn, &event).unwrap();
    }
    model::set_event_field(
        &mut txn,
        &uuid(3),
        &model::EventField::Status(Status::Rejected),
    )
    .unwrap();
    model::delete_event(&mut txn, &uuid(4));
    drop(txn);
    doc
}

fn catalogue(doc: &Doc, uuid: &str, listed: &[String], rule: Option<Value>) {
    let mut txn = doc.transact_mut();
    let new = NewCatalogue {
        uuid: uuid.to_owned(),
        name: uuid.to_owned(),
        ..NewCatalogue::default()
    };
    model::create_catalogue(&mut txn, &new).unwrap();
    model::add_catalogue_events(&mut txn, uuid, listed);
    model::set_catalogue_rule(&mut txn, uuid, rule.as_ref()).unwrap();
}

fn filter(predicate: Value) -> Value {
    json!({"filter": {"all": true}, "where": predicate})
}

fn members(doc: &Doc, uuid: &str) -> Result<Vec<Member>, RuleError> {
    model::catalogue_members(&doc.transact(), uuid).unwrap()
}

fn event_uuids(doc: &Doc, uuid: &str) -> Vec<String> {
    let events = model::catalogue_events(&doc.transact(), uuid)
        .unwrap()
        .unwrap();
    events.into_iter().map(|event| event.uuid).collect()
}

fn matching(predicate: Value) -> Vec<String> {
    let doc = room();
    catalogue(&doc, SMART, &[], Some(filter(predicate)));
    event_uuids(&doc, SMART)
}

fn uuids(numbers: &[u32]) -> Vec<String> {
    numbers.iter().map(|n| uuid(*n)).collect()
}

#[test]
fn a_catalogue_without_a_rule_has_its_live_listed_events() {
    let doc = room();
    catalogue(&doc, SMART, &uuids(&[2, 4, 9]), None);
    assert_eq!(event_uuids(&doc, SMART), uuids(&[2]));
}

#[test]
fn members_are_the_listed_events_plus_the_rule_result_each_once() {
    let doc = room();
    let listed = uuids(&[1, 2]);
    catalogue(
        &doc,
        SMART,
        &listed,
        Some(filter(json!({"cmp": ["#q", ">=", 2]}))),
    );
    assert_eq!(event_uuids(&doc, SMART), uuids(&[1, 2, 3]));
}

#[test]
fn all_never_yields_a_deleted_event_but_keeps_a_rejected_one() {
    assert_eq!(matching(json!({"and": []})), uuids(&[1, 2, 3]));
}

#[test]
fn a_time_field_compares_as_nanoseconds() {
    let start = BASE + 20 * SECOND;
    assert_eq!(
        matching(json!({"cmp": ["start", ">=", start]})),
        uuids(&[2, 3])
    );
    assert_eq!(matching(json!({"cmp": ["stop", "<", start]})), uuids(&[1]));
}

#[test]
fn a_missing_field_matches_no_comparison() {
    assert_eq!(
        matching(json!({"cmp": ["#absent", "!=", 1]})),
        Vec::<String>::new()
    );
    assert_eq!(
        matching(json!({"cmp": ["confidence", "<", 1]})),
        Vec::<String>::new()
    );
}

#[test]
fn a_comparison_with_null_asks_whether_the_field_is_unset() {
    assert_eq!(
        matching(json!({"cmp": ["confidence", "==", null]})),
        uuids(&[1, 2, 3])
    );
    assert_eq!(
        matching(json!({"cmp": ["confidence", "!=", null]})),
        Vec::<String>::new()
    );
    assert_eq!(
        matching(json!({"cmp": ["#q", "!=", null]})),
        uuids(&[1, 2, 3])
    );
    assert_eq!(
        matching(json!({"cmp": ["#q", ">", null]})),
        Vec::<String>::new()
    );
}

#[test]
fn a_date_attribute_stored_as_text_compares_as_a_time() {
    let doc = room();
    for (n, seen) in [
        (1, "2025-01-31T10:00:05Z"),
        (2, "2025-01-31T11:00:00+01:00"),
    ] {
        let seen = object(json!({"seen": seen}));
        model::set_event_attributes(&mut doc.transact_mut(), &uuid(n), &seen).unwrap();
    }
    let rule = filter(json!({"cmp": ["#seen", ">", BASE + 3 * SECOND]}));
    catalogue(&doc, SMART, &[], Some(rule));
    assert_eq!(event_uuids(&doc, SMART), uuids(&[1]));
    let rule = filter(json!({"cmp": ["#seen", "==", BASE]}));
    catalogue(&doc, OTHER, &[], Some(rule));
    assert_eq!(event_uuids(&doc, OTHER), uuids(&[2]));
}

#[test]
fn status_reads_as_confirmed_when_unset() {
    assert_eq!(
        matching(json!({"cmp": ["status", "==", "confirmed"]})),
        uuids(&[1, 2])
    );
    assert_eq!(
        matching(json!({"cmp": ["status", "==", "rejected"]})),
        uuids(&[3])
    );
}

#[test]
fn each_predicate_reads_the_room() {
    assert_eq!(
        matching(json!({"cmp": ["author", "==", "bob"]})),
        uuids(&[2])
    );
    assert_eq!(
        matching(json!({"cmp": ["uuid", "==", uuid(3)]})),
        uuids(&[3])
    );
    assert_eq!(
        matching(json!({"match": ["#label", "-[12]$"]})),
        uuids(&[1, 2])
    );
    assert_eq!(matching(json!({"has": "author"})), uuids(&[1, 2, 3]));
    assert_eq!(matching(json!({"has": "origin"})), Vec::<String>::new());
    assert_eq!(matching(json!({"contains": ["tags", "t2"]})), uuids(&[2]));
    assert_eq!(
        matching(json!({"not": {"cmp": ["#q", "==", 1]}})),
        uuids(&[2, 3])
    );
    assert_eq!(
        matching(json!({"or": [{"cmp": ["#q", "==", 1]}, {"cmp": ["#q", "==", 3.0]}]})),
        uuids(&[1, 3])
    );
    assert_eq!(
        matching(json!({"and": [{"has": "#q"}, {"match": ["author", "^a"]}]})),
        uuids(&[1, 3])
    );
}

#[test]
fn in_catalogue_reads_a_smart_catalogues_members() {
    let doc = room();
    catalogue(
        &doc,
        OTHER,
        &uuids(&[1]),
        Some(filter(json!({"cmp": ["#q", "==", 3]}))),
    );
    catalogue(
        &doc,
        SMART,
        &[],
        Some(filter(json!({"in_catalogue": OTHER}))),
    );
    assert_eq!(event_uuids(&doc, SMART), uuids(&[1, 3]));
}

#[test]
fn in_no_catalogue_means_listed_by_none_alive() {
    let doc = room();
    catalogue(&doc, OTHER, &uuids(&[1]), None);
    catalogue(&doc, THIRD, &uuids(&[2]), None);
    model::delete_catalogue(&mut doc.transact_mut(), THIRD);
    catalogue(
        &doc,
        SMART,
        &[],
        Some(filter(json!({"in_catalogue": null}))),
    );
    assert_eq!(event_uuids(&doc, SMART), uuids(&[2, 3]));
}

#[test]
fn a_catalogue_set_reads_another_catalogues_members() {
    let doc = room();
    catalogue(
        &doc,
        OTHER,
        &uuids(&[2]),
        Some(filter(json!({"cmp": ["#q", "==", 1]}))),
    );
    catalogue(&doc, SMART, &[], Some(json!({"catalogue": OTHER})));
    assert_eq!(event_uuids(&doc, SMART), uuids(&[1, 2]));
}

#[test]
fn a_missing_or_deleted_catalogue_is_an_empty_set() {
    let doc = room();
    catalogue(&doc, OTHER, &uuids(&[1]), None);
    model::delete_catalogue(&mut doc.transact_mut(), OTHER);
    let rule = json!({"union": [{"catalogue": OTHER}, {"catalogue": THIRD}]});
    catalogue(&doc, SMART, &uuids(&[2]), Some(rule));
    assert_eq!(event_uuids(&doc, SMART), uuids(&[2]));
}

#[test]
fn a_cycle_is_an_error() {
    let doc = room();
    catalogue(&doc, OTHER, &[], Some(json!({"catalogue": SMART})));
    catalogue(&doc, SMART, &[], Some(json!({"catalogue": OTHER})));
    let error = members(&doc, SMART).unwrap_err();
    assert!(
        matches!(&error, RuleError::Cycle(uuid) if uuid == SMART),
        "{error:?}"
    );
}

#[test]
fn a_cycle_through_in_catalogue_is_an_error() {
    let doc = room();
    catalogue(
        &doc,
        SMART,
        &[],
        Some(filter(json!({"in_catalogue": SMART}))),
    );
    assert!(matches!(members(&doc, SMART), Err(RuleError::Cycle(_))));
}

#[test]
fn reaching_a_catalogue_twice_without_a_cycle_is_fine() {
    let doc = room();
    catalogue(&doc, OTHER, &uuids(&[1]), None);
    let rule = json!({"union": [{"catalogue": OTHER}, {"catalogue": OTHER}]});
    catalogue(&doc, SMART, &[], Some(rule));
    assert_eq!(event_uuids(&doc, SMART), uuids(&[1]));
}

#[test]
fn an_interval_operator_yields_derived_members() {
    let doc = room();
    catalogue(
        &doc,
        SMART,
        &[],
        Some(json!({"merge": {"all": true}, "gap": 9})),
    );
    let members = members(&doc, SMART).unwrap();
    assert_eq!(
        members,
        vec![Member::Derived {
            start: at(10),
            stop: at(31),
            sources: uuids(&[1, 2, 3]),
        }]
    );
    assert_eq!(event_uuids(&doc, SMART), Vec::<String>::new());
}

#[test]
fn closest_gives_each_event_its_gap_in_seconds() {
    let doc = room();
    catalogue(&doc, OTHER, &uuids(&[3]), None);
    let rule = json!({"closest": [{"catalogue": OTHER}, {"filter": {"all": true}, "where": {"cmp": ["#q", "==", 1]}}]});
    catalogue(&doc, SMART, &[], Some(rule));
    let members = members(&doc, SMART).unwrap();
    let [Member::Event { record, gap }] = members.as_slice() else {
        panic!("{members:?}")
    };
    assert_eq!(record.uuid, uuid(3));
    assert_eq!(*gap, Some(19.0));
}

#[test]
fn an_unknown_catalogue_has_no_members() {
    assert!(model::catalogue_members(&room().transact(), SMART).is_none());
    assert!(model::catalogue_events(&room().transact(), SMART).is_none());
}

#[test]
fn an_invalid_rule_is_refused_on_write() {
    let doc = room();
    catalogue(&doc, SMART, &[], None);
    let bad = json!({"filter": {"all": true}, "where": {"match": ["author", "("]}});
    let error = model::set_catalogue_rule(&mut doc.transact_mut(), SMART, Some(&bad)).unwrap_err();
    assert!(
        matches!(
            error,
            WriteError::Rule(RuleError::Invalid {
                problem: Problem::Regex { .. },
                ..
            })
        ),
        "{error:?}"
    );
    assert_eq!(model::catalogue(&doc.transact(), SMART).unwrap().rule, None);
}

#[test]
fn an_invalid_rule_in_the_room_is_an_evaluation_error() {
    let doc = room();
    catalogue(&doc, SMART, &[], None);
    {
        let mut txn = doc.transact_mut();
        let catalogues = txn.get_map(model::CATALOGUES).unwrap();
        let Some(Out::YMap(map)) = catalogues.get(&txn, SMART) else {
            panic!("no catalogue")
        };
        map.insert(&mut txn, "rule", Any::from("not a rule"));
    }
    let error = members(&doc, SMART).unwrap_err();
    assert_eq!(
        error.to_string(),
        "rule: a set is a JSON object, not \"not a rule\""
    );
    assert!(
        model::catalogue_events(&doc.transact(), SMART)
            .unwrap()
            .is_err()
    );
}

#[test]
fn materialize_copies_events_and_makes_derived_intervals_events_of_a_rule_origin() {
    let doc = room();
    let rule = json!({"union": [
        {"filter": {"all": true}, "where": {"cmp": ["#q", "==", 1]}},
        {"merge": {"filter": {"all": true}, "where": {"cmp": ["#q", ">=", 2]}}, "gap": 9}
    ]});
    catalogue(&doc, SMART, &[], Some(rule.clone()));
    let copy = NewCatalogue {
        uuid: OTHER.to_owned(),
        name: "copy".to_owned(),
        author: Some("carol".to_owned()),
        ..NewCatalogue::default()
    };
    assert!(model::materialize(&mut doc.transact_mut(), SMART, &copy, hop()).unwrap());

    let txn = doc.transact();
    let summary = model::catalogue(&txn, OTHER).unwrap();
    assert_eq!((summary.name.as_str(), summary.rule), ("copy", None));
    let events = model::catalogue_events(&txn, OTHER).unwrap().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].uuid, uuid(1));
    let made = &events[1];
    assert_eq!((made.start, made.stop), (at(20), at(31)));
    let origin = model::origin(&txn, made.origin.as_deref().unwrap()).unwrap();
    assert_eq!(origin.kind, OriginKind::Rule);
    assert_eq!(origin.name, SMART);
    assert_eq!(origin.author.as_deref(), Some("carol"));
    assert_eq!(origin.parameters, Some(rule));
    assert_eq!(origin.derived_from.as_deref(), Some(SMART));
}

#[test]
fn materialize_of_an_unknown_catalogue_writes_nothing() {
    let doc = room();
    let copy = NewCatalogue {
        uuid: OTHER.to_owned(),
        ..NewCatalogue::default()
    };
    assert!(!model::materialize(&mut doc.transact_mut(), SMART, &copy, hop()).unwrap());
    assert!(model::catalogue(&doc.transact(), OTHER).is_none());
}

#[test]
fn materialize_without_derived_intervals_makes_no_origin() {
    let doc = room();
    catalogue(&doc, SMART, &uuids(&[1]), None);
    let copy = NewCatalogue {
        uuid: OTHER.to_owned(),
        ..NewCatalogue::default()
    };
    assert!(model::materialize(&mut doc.transact_mut(), SMART, &copy, hop()).unwrap());
    let txn = doc.transact();
    assert_eq!(event_uuids(&doc, OTHER), uuids(&[1]));
    assert!(txn.get_map(model::ORIGINS).is_none());
}

#[test]
fn a_frozen_catalogue_names_the_smart_one_it_froze() {
    let doc = room();
    catalogue(&doc, SMART, &uuids(&[1]), None);
    let earlier = Hop {
        catalogue: "earlier".to_owned(),
        ..hop()
    };
    model::set_copied_from(
        &mut doc.transact_mut(),
        SMART,
        std::slice::from_ref(&earlier),
    );
    catalogue(&doc, THIRD, &[], None);
    let copy = NewCatalogue {
        uuid: OTHER.to_owned(),
        ..NewCatalogue::default()
    };
    assert!(model::materialize(&mut doc.transact_mut(), SMART, &copy, hop()).unwrap());

    let frozen = model::catalogue(&doc.transact(), OTHER).unwrap();
    assert_eq!(frozen.copied_from, [earlier, hop()]);
    let untouched = model::catalogue(&doc.transact(), THIRD).unwrap();
    assert!(untouched.copied_from.is_empty());
}

#[test]
fn a_search_of_the_public_catalogues_holds_only_its_picked_events_in_the_room() {
    let doc = room();
    let rule = json!({"filter": {"public": true}, "where": {"text": "shock"}});
    catalogue(&doc, SMART, &uuids(&[2]), Some(rule));
    assert_eq!(event_uuids(&doc, SMART), uuids(&[2]));
}

#[test]
fn an_unsaved_rule_previews_how_many_it_holds_and_the_first_ones() {
    let doc = room();
    catalogue(&doc, OTHER, &uuids(&[1, 2, 3]), None);
    let txn = doc.transact();
    let all = filter(json!({"and": []}));
    let preview = model::rule_members_at(&txn, &all, Timestamp::now(), None).unwrap();
    assert_eq!(preview.count, 3);
    let firsts: Vec<String> = preview
        .members
        .iter()
        .map(|member| match member {
            Member::Event { record, .. } => record.uuid.clone(),
            Member::Derived { .. } => panic!("no derived interval here"),
        })
        .collect();
    assert_eq!(firsts, uuids(&[1, 2, 3]), "in time order");
    let preview = model::rule_members_at(&txn, &all, Timestamp::now(), Some(1)).unwrap();
    assert_eq!((preview.count, preview.members.len()), (3, 1));
    let alice = json!({"filter": {"catalogue": OTHER}, "where": {"match": ["author", "alice"]}});
    let preview = model::rule_members_at(&txn, &alice, Timestamp::now(), None).unwrap();
    assert_eq!(preview.count, 2);
    let merged = json!({"merge": {"all": true}, "gap": 60});
    let preview = model::rule_members_at(&txn, &merged, Timestamp::now(), None).unwrap();
    assert!(matches!(preview.members[..], [Member::Derived { .. }]));
    let broken = json!({"filter": {"all": true}, "where": {"nonsense": 1}});
    assert!(model::rule_members_at(&txn, &broken, Timestamp::now(), None).is_err());
    drop(txn);
    assert_eq!(
        model::catalogue(&doc.transact(), OTHER).unwrap().rule,
        None,
        "nothing saved"
    );
}

fn probes(numbers: &[u32]) -> Vec<model::Probe> {
    numbers
        .iter()
        .map(|n| model::Probe::Event(uuid(*n)))
        .collect()
}

fn mask(predicate: &str, source: Option<&str>, probes: &[model::Probe]) -> Vec<bool> {
    let doc = room();
    catalogue(&doc, OTHER, &uuids(&[2, 3]), None);
    let txn = doc.transact();
    let names = model::catalogue_names(&txn);
    let predicate = cocat_core::query::parse(predicate, &names).unwrap();
    model::search_mask(&txn, &predicate, source, Timestamp::now(), probes).unwrap()
}

#[test]
fn a_search_mask_says_what_a_smart_catalogue_with_that_search_would_hold() {
    let shown = probes(&[3, 1, 4, 2, 99]);
    assert_eq!(
        mask("author:alice", None, &shown),
        [true, true, false, false, false],
        "aligned to the probes; an event in the trash or not in the room never matches"
    );
    assert_eq!(
        mask("author:alice", Some(OTHER), &shown),
        [true, false, false, false, false],
        "with a source, only its members"
    );
    assert_eq!(mask("", Some("nowhere"), &shown), [false; 5]);
    let interval = model::Probe::Interval {
        start: BASE + 15 * SECOND,
        stop: BASE + 16 * SECOND,
    };
    let later = format!("start:>={}", at(12).rfc3339().unwrap());
    assert_eq!(
        mask(&later, None, std::slice::from_ref(&interval)),
        [true],
        "a derived interval is tested by its range"
    );
    assert_eq!(mask("tag:t1", None, &[interval]), [false]);
}

#[test]
fn the_words_filter_reads_what_the_table_shows() {
    let doc = room();
    let txn = doc.transact();
    let shown = probes(&[1, 2, 3, 4, 99]);
    let words = |text: &str| model::words_mask(&txn, text, &shown);
    assert_eq!(words("ALICE"), [true, false, true, false, false]);
    assert_eq!(words("t2"), [false, true, false, false, false]);
    assert_eq!(words("rejected ev-3"), [false, false, true, false, false]);
    assert_eq!(
        words("2025-01-31 10:00:2"),
        [false, true, false, false, false]
    );
    assert_eq!(
        words(" "),
        [true, true, true, true, false],
        "the trash is shown too"
    );
    let interval = model::Probe::Interval {
        start: BASE,
        stop: BASE + SECOND,
    };
    assert_eq!(model::words_mask(&txn, "2025-01-31", &[interval]), [true]);
}

#[test]
fn the_vocabulary_is_what_a_search_can_name() {
    let doc = room();
    catalogue(&doc, OTHER, &[], None);
    let vocabulary = model::vocabulary(&doc.transact());
    assert_eq!(vocabulary.authors, ["alice", "bob"]);
    assert_eq!(vocabulary.catalogues, [OTHER]);
    assert_eq!(vocabulary.attributes, ["label", "q"]);
    assert!(vocabulary.tags.contains(&"t1".to_owned()));
}

/// The names and authors a search box needs are read without reading whole catalogues and
/// events (a project of 100,000 events made them slow), and are the same as before.
#[test]
fn names_and_authors_read_fast_are_those_of_the_full_records() {
    let doc = room();
    catalogue(&doc, THIRD, &[uuid(1), uuid(2)], None);
    catalogue(&doc, OTHER, &[uuid(3)], None);
    catalogue(&doc, SMART, &[], None);
    {
        let mut txn = doc.transact_mut();
        model::rename_catalogue(&mut txn, THIRD, "Same");
        model::rename_catalogue(&mut txn, OTHER, "Same");
        model::delete_catalogue(&mut txn, SMART);
        let mut gone = EventRecord::new(uuid(5), at(50), at(51));
        gone.author = Some("dora".to_owned());
        model::create_event(&mut txn, &gone).unwrap();
        model::delete_event(&mut txn, &uuid(5));
        let events = yrs::WriteTxn::get_or_insert_map(&mut txn, model::EVENTS);
        let no_range = yrs::MapPrelim::from([("author".to_owned(), yrs::In::from("eve"))]);
        events.insert(&mut txn, uuid(6), no_range);
    }
    let txn = doc.transact();
    let named: Vec<(String, String)> = model::catalogues(&txn)
        .into_iter()
        .map(|catalogue| (catalogue.uuid, catalogue.name))
        .collect();
    assert_eq!(model::catalogue_names(&txn), named);
    assert_eq!(named.len(), 2);
    let has_author = cocat_core::rules::Predicate::Has("author".to_owned());
    let mut authors: Vec<String> = model::search(&txn, &has_author, Timestamp::now())
        .unwrap()
        .into_iter()
        .filter_map(|event| event.author)
        .collect();
    authors.sort();
    authors.dedup();
    assert_eq!(model::vocabulary(&txn).authors, authors);
    assert_eq!(authors, ["alice", "bob"]);
}
