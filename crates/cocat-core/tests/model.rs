//! Reading and writing the v2 room layout.

use std::collections::BTreeSet;

use cocat_core::model::{
    self, AttributeType, EventField, EventRecord, NewCatalogue, Origin, OriginKind, Status, Used,
    Vertex, WriteError,
};
use cocat_core::time::Timestamp;
use serde_json::{Map, Value, json};
use yrs::{Any, Doc, Map as _, Out, ReadTxn as _, Transact};

const ALPHA: &str = "6f9c1a52-0b0d-4f4e-9d5e-1b2c3d4e5f60";
const BETA: &str = "0e4d2c31-5a6b-4c7d-8e9f-0a1b2c3d4e5f";
const EVENT: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const OTHER: &str = "bbbbbbbb-2222-4222-8222-222222222222";
const UNKNOWN: &str = "99999999-8888-4777-8666-555555555555";

fn at(text: &str) -> Timestamp {
    Timestamp::parse(text).unwrap()
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

fn names(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn hand_made(uuid: &str) -> EventRecord {
    let mut event = EventRecord::new(uuid, at("2025-01-31 10:00:00"), at("2025-01-31 10:01:00"));
    event.author = Some("a@x.org".to_owned());
    event.created = Some(at("2025-02-01 08:00:00.123456"));
    event.tags = vec!["t1".to_owned(), "t2".to_owned()];
    event.products = vec!["p1".to_owned()];
    event.attributes = object(json!({"q": 1.5, "flag": true}));
    event
}

fn alpha() -> NewCatalogue {
    NewCatalogue {
        uuid: ALPHA.to_owned(),
        name: "alpha".to_owned(),
        author: Some("a@x.org".to_owned()),
        tags: vec!["c1".to_owned()],
        columns: vec!["q".to_owned(), "flag".to_owned()],
        attributes: object(json!({"mission": "MMS", "level": 2})),
        ..NewCatalogue::default()
    }
}

/// A room holding catalogue alpha, which lists one hand-made event.
fn written() -> Doc {
    let doc = Doc::new();
    {
        let mut txn = doc.transact_mut();
        model::create_catalogue(&mut txn, &alpha()).unwrap();
        model::create_event(&mut txn, &hand_made(EVENT)).unwrap();
        assert!(model::add_catalogue_events(
            &mut txn,
            ALPHA,
            &[EVENT.to_owned()]
        ));
    }
    doc
}

fn stored_keys(doc: &Doc, root: &str, uuid: &str) -> BTreeSet<String> {
    let txn = doc.transact();
    let Some(Out::YMap(map)) = txn.get_map(root).unwrap().get(&txn, uuid) else {
        panic!("{uuid} is missing from {root}");
    };
    map.keys(&txn).map(str::to_owned).collect()
}

fn stored(doc: &Doc, uuid: &str, key: &str) -> Option<Out> {
    let txn = doc.transact();
    let Some(Out::YMap(map)) = txn.get_map(model::EVENTS).unwrap().get(&txn, uuid) else {
        panic!("{uuid} is missing");
    };
    map.get(&txn, key)
}

#[test]
fn an_event_reads_back_as_written() {
    let doc = Doc::new();
    let mut event = hand_made(EVENT);
    event.origin = Some("run-1".to_owned());
    event.status = Status::Reviewed;
    event.confidence = Some(0.7);
    event.start_err = Some(0.5);
    event.stop_err = Some(1.5);
    event.source_id = Some("list-42".to_owned());
    model::create_event(&mut doc.transact_mut(), &event).unwrap();

    assert_eq!(model::event(&doc.transact(), EVENT), Some(event));
}

#[test]
fn a_catalogue_reads_back_as_written() {
    let doc = written();
    let txn = doc.transact();

    let catalogue = model::catalogue(&txn, ALPHA).unwrap();
    assert_eq!(catalogue.name, "alpha");
    assert_eq!(catalogue.author.as_deref(), Some("a@x.org"));
    assert_eq!(catalogue.tags, ["c1"]);
    assert_eq!(catalogue.columns, ["q", "flag"]);
    assert_eq!(
        catalogue.attributes,
        object(json!({"mission": "MMS", "level": 2}))
    );
    assert_eq!(catalogue.rule, None);
    assert!(!catalogue.deleted);
    assert_eq!(catalogue.event_count, 1);
    assert_eq!(model::catalogues(&txn), vec![catalogue]);
    assert_eq!(
        model::catalogue_events(&txn, ALPHA).unwrap().unwrap(),
        vec![hand_made(EVENT)]
    );
}

#[test]
fn an_event_stores_only_what_differs_from_the_defaults() {
    let doc = Doc::new();
    let bare = EventRecord::new(EVENT, at("2025-01-31 10:00:00"), at("2025-01-31 10:01:00"));
    model::create_event(&mut doc.transact_mut(), &bare).unwrap();
    model::create_event(&mut doc.transact_mut(), &hand_made(OTHER)).unwrap();

    assert_eq!(
        stored_keys(&doc, model::EVENTS, EVENT),
        names(&["start", "stop"])
    );
    assert_eq!(
        stored_keys(&doc, model::EVENTS, OTHER),
        names(&[
            "#flag", "#q", "author", "created", "products", "start", "stop", "tags"
        ])
    );
}

#[test]
fn times_are_nanoseconds_and_tags_a_sorted_array() {
    let doc = Doc::new();
    let mut event = hand_made(EVENT);
    event.tags = vec!["b".to_owned(), "a".to_owned(), "b".to_owned()];
    model::create_event(&mut doc.transact_mut(), &event).unwrap();

    let start = at("2025-01-31 10:00:00").unix_nanos().unwrap();
    assert_eq!(start, 1_738_317_600_000_000_000);
    let Some(Out::Any(Any::Number(number))) = stored(&doc, EVENT, "start") else {
        panic!("the start is not a number");
    };
    assert_eq!(number.as_i64(), Some(start));
    assert_eq!(
        stored(&doc, EVENT, "tags"),
        Some(Out::Any(Any::Array(
            vec![Any::from("a"), Any::from("b")].into()
        )))
    );
    assert_eq!(
        model::event(&doc.transact(), EVENT).unwrap().tags,
        ["a", "b"]
    );
}

#[test]
fn the_first_write_marks_the_layout() {
    let doc = written();
    let txn = doc.transact();
    let meta = txn.get_map(model::META).unwrap();
    assert_eq!(
        meta.get(&txn, "layout"),
        Some(Out::Any(Any::from(model::LAYOUT)))
    );
    assert!(!model::is_v1(&txn));
    assert!(!model::is_v1(&Doc::new().transact()));
}

#[test]
fn fields_change_and_go_back_to_their_default() {
    let doc = written();
    {
        let mut txn = doc.transact_mut();
        for field in [
            EventField::Status(Status::Final),
            EventField::Confidence(Some(0.25)),
            EventField::SourceId(Some("x".to_owned())),
            EventField::Tags(vec!["new".to_owned()]),
        ] {
            assert!(model::set_event_field(&mut txn, EVENT, &field).unwrap());
        }
    }
    let event = model::event(&doc.transact(), EVENT).unwrap();
    assert_eq!(event.status, Status::Final);
    assert_eq!(event.confidence, Some(0.25));
    assert_eq!(event.source_id.as_deref(), Some("x"));
    assert_eq!(event.tags, ["new"]);

    {
        let mut txn = doc.transact_mut();
        for field in [
            EventField::Status(Status::Confirmed),
            EventField::Confidence(None),
            EventField::SourceId(None),
            EventField::Tags(Vec::new()),
        ] {
            assert!(model::set_event_field(&mut txn, EVENT, &field).unwrap());
        }
    }
    let keys = stored_keys(&doc, model::EVENTS, EVENT);
    for gone in ["status", "confidence", "source_id", "tags"] {
        assert!(!keys.contains(gone), "{gone} is still stored: {keys:?}");
    }
}

#[test]
fn a_confidence_outside_zero_to_one_is_refused() {
    let doc = written();
    let error = model::set_event_field(
        &mut doc.transact_mut(),
        EVENT,
        &EventField::Confidence(Some(1.5)),
    )
    .unwrap_err();
    assert!(matches!(error, WriteError::Confidence(value) if value == 1.5));

    let mut event = hand_made(OTHER);
    event.confidence = Some(-0.1);
    assert!(model::create_event(&mut doc.transact_mut(), &event).is_err());
}

#[test]
fn a_shape_sets_the_range_to_its_time_span() {
    let doc = written();
    let vertex = |time: &str, frequency: f64| Vertex {
        time: at(time),
        frequency,
    };
    let shape = vec![
        vertex("2025-03-01 00:00:10", 1e3),
        vertex("2025-03-01 00:00:00", 2e3),
        vertex("2025-03-01 00:00:30", 1e3),
    ];
    model::set_event_field(
        &mut doc.transact_mut(),
        EVENT,
        &EventField::Shape(Some(shape.clone())),
    )
    .unwrap();

    let event = model::event(&doc.transact(), EVENT).unwrap();
    assert_eq!(event.shape, Some(shape.clone()));
    assert_eq!(event.start, at("2025-03-01 00:00:00"));
    assert_eq!(event.stop, at("2025-03-01 00:00:30"));

    let mut created = hand_made(OTHER);
    created.shape = Some(shape);
    model::create_event(&mut doc.transact_mut(), &created).unwrap();
    let read = model::event(&doc.transact(), OTHER).unwrap();
    assert_eq!((read.start, read.stop), (event.start, event.stop));

    let error = model::set_event_field(
        &mut doc.transact_mut(),
        EVENT,
        &EventField::Shape(Some(Vec::new())),
    )
    .unwrap_err();
    assert!(matches!(error, WriteError::EmptyShape));
}

#[test]
fn attributes_are_flat_keys_that_merge_and_go() {
    let doc = written();
    {
        let mut txn = doc.transact_mut();
        let warnings = model::set_event_attributes(&mut txn, EVENT, &object(json!({"note": "ok"})))
            .unwrap()
            .unwrap();
        assert!(warnings.is_empty());
        assert!(model::remove_event_attributes(
            &mut txn,
            EVENT,
            &["q".to_owned()]
        ));
        let level = object(json!({"level": 3}));
        assert!(model::set_catalogue_attributes(&mut txn, ALPHA, &level).unwrap());
        assert!(model::remove_catalogue_attributes(
            &mut txn,
            ALPHA,
            &["mission".to_owned()]
        ));
    }
    let txn = doc.transact();
    assert_eq!(
        model::event(&txn, EVENT).unwrap().attributes,
        object(json!({"flag": true, "note": "ok"}))
    );
    assert_eq!(
        model::catalogue(&txn, ALPHA).unwrap().attributes,
        object(json!({"level": 3}))
    );
    drop(txn);
    assert!(stored_keys(&doc, model::EVENTS, EVENT).contains("#note"));
}

#[test]
fn a_new_attribute_registers_its_type() {
    let doc = written();
    let txn = doc.transact();
    let types = model::attribute_types(&txn);
    assert_eq!(types["q"].kind, AttributeType::Number);
    assert_eq!(types["flag"].kind, AttributeType::Bool);
    assert_eq!(types.len(), 2, "catalogue attributes stay out: {types:?}");
}

#[test]
fn a_value_of_another_type_is_written_with_a_warning() {
    let doc = written();
    let warnings = model::set_event_attributes(
        &mut doc.transact_mut(),
        EVENT,
        &object(json!({"q": "high"})),
    )
    .unwrap()
    .unwrap();

    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].key, "q");
    assert_eq!(warnings[0].registered, AttributeType::Number);
    let txn = doc.transact();
    assert_eq!(
        model::event(&txn, EVENT).unwrap().attributes["q"],
        json!("high")
    );
    assert_eq!(
        model::attribute_types(&txn)["q"].kind,
        AttributeType::Number
    );
}

#[test]
fn a_date_attribute_takes_iso_text() {
    let doc = written();
    let mut txn = doc.transact_mut();
    model::register_attribute(&mut txn, "seen", AttributeType::Date);
    let iso = object(json!({"seen": "2025-01-31T10:00:00Z"}));
    let fine = model::set_event_attributes(&mut txn, EVENT, &iso)
        .unwrap()
        .unwrap();
    let soon = object(json!({"seen": "soon"}));
    let wrong = model::set_event_attributes(&mut txn, EVENT, &soon)
        .unwrap()
        .unwrap();

    assert!(fine.is_empty());
    assert_eq!(wrong.len(), 1);
}

#[test]
fn a_registered_key_gets_a_unit_and_a_description() {
    let doc = written();
    {
        let mut txn = doc.transact_mut();
        assert!(model::set_attribute_info(
            &mut txn,
            "q",
            Some("nT"),
            Some("quality")
        ));
        assert!(model::set_attribute_info(&mut txn, "q", None, Some("")));
        assert!(!model::set_attribute_info(
            &mut txn,
            "nope",
            Some("s"),
            None
        ));
    }
    let info = &model::attribute_types(&doc.transact())["q"];
    assert_eq!(info.unit.as_deref(), Some("nT"));
    assert_eq!(info.description, None);
}

#[test]
fn a_deleted_event_goes_to_the_trash_and_comes_back() {
    let doc = written();
    let beta = NewCatalogue {
        uuid: BETA.to_owned(),
        name: "beta".to_owned(),
        ..NewCatalogue::default()
    };
    {
        let mut txn = doc.transact_mut();
        model::create_catalogue(&mut txn, &beta).unwrap();
        model::add_catalogue_events(&mut txn, BETA, &[EVENT.to_owned()]);
        assert!(model::delete_event(&mut txn, EVENT));
    }
    let txn = doc.transact();
    assert_eq!(
        model::catalogue_events(&txn, ALPHA).unwrap().unwrap(),
        Vec::new()
    );
    assert_eq!(model::catalogue(&txn, ALPHA).unwrap().event_count, 0);
    assert_eq!(model::live_event_count(&txn), 0);
    assert_eq!(model::event(&txn, EVENT).unwrap().status, Status::Deleted);
    let trash: Vec<String> = model::trash(&txn)
        .into_iter()
        .map(|event| event.uuid)
        .collect();
    assert_eq!(trash, [EVENT]);
    drop(txn);

    assert!(model::restore_event(&mut doc.transact_mut(), EVENT));
    let txn = doc.transact();
    assert_eq!(
        model::catalogue_events(&txn, ALPHA).unwrap().unwrap().len(),
        1
    );
    assert_eq!(model::event_catalogues(&txn, EVENT), [BETA, ALPHA]);
    assert!(model::trash(&txn).is_empty());
    assert_eq!(model::live_event_count(&txn), 1);
}

#[test]
fn purging_an_event_removes_it_everywhere() {
    let doc = written();
    {
        let mut txn = doc.transact_mut();
        assert!(model::set_comment(&mut txn, EVENT, Some("a long story")));
        assert!(model::purge_event(&mut txn, EVENT));
        assert!(!model::purge_event(&mut txn, EVENT));
    }
    let txn = doc.transact();
    assert_eq!(model::event(&txn, EVENT), None);
    assert_eq!(model::comment(&txn, EVENT), None);
    let catalogues = txn.get_map(model::CATALOGUES).unwrap();
    let Some(Out::YMap(alpha)) = catalogues.get(&txn, ALPHA) else {
        panic!("alpha is missing");
    };
    let Some(Out::YMap(listed)) = alpha.get(&txn, "events") else {
        panic!("alpha lists nothing");
    };
    assert_eq!(listed.len(&txn), 0);
}

#[test]
fn a_deleted_catalogue_is_hidden_until_restored_and_purging_keeps_its_events() {
    let doc = written();
    assert!(model::delete_catalogue(&mut doc.transact_mut(), ALPHA));
    {
        let txn = doc.transact();
        assert_eq!(model::catalogues(&txn), Vec::new());
        assert!(model::catalogue(&txn, ALPHA).unwrap().deleted);
        assert!(model::event_catalogues(&txn, EVENT).is_empty());
    }
    assert!(model::restore_catalogue(&mut doc.transact_mut(), ALPHA));
    assert_eq!(model::catalogues(&doc.transact()).len(), 1);

    assert!(model::purge_catalogue(&mut doc.transact_mut(), ALPHA));
    let txn = doc.transact();
    assert_eq!(model::catalogue(&txn, ALPHA), None);
    assert!(model::event(&txn, EVENT).is_some());
}

#[test]
fn comments_live_outside_the_event() {
    let doc = written();
    {
        let mut txn = doc.transact_mut();
        assert!(model::set_comment(&mut txn, EVENT, Some("first")));
        assert!(!model::set_comment(&mut txn, UNKNOWN, Some("nobody")));
    }
    assert_eq!(
        model::comment(&doc.transact(), EVENT).as_deref(),
        Some("first")
    );
    assert!(!stored_keys(&doc, model::EVENTS, EVENT).contains("comment"));

    model::set_comment(&mut doc.transact_mut(), EVENT, None);
    assert_eq!(model::comment(&doc.transact(), EVENT), None);
}

#[test]
fn an_origin_reads_back_and_a_model_event_names_it() {
    let doc = Doc::new();
    let origin = Origin {
        id: "run-1".to_owned(),
        kind: OriginKind::Model,
        name: "shock finder".to_owned(),
        version: Some("1.2".to_owned()),
        author: Some("a@x.org".to_owned()),
        created: Some(at("2025-01-01 00:00:00")),
        used: vec![Used {
            product: "amda/mms1_fgm".to_owned(),
            version: None,
            retrieved: Some("2025-01-01".to_owned()),
        }],
        parameters: Some(json!({"threshold": 3})),
        derived_from: None,
    };
    let mut event = EventRecord::new(EVENT, at("2025-01-31 10:00:00"), at("2025-01-31 10:01:00"));
    event.origin = Some("run-1".to_owned());
    event.confidence = Some(0.9);
    {
        let mut txn = doc.transact_mut();
        model::create_origin(&mut txn, &origin).unwrap();
        model::create_event(&mut txn, &event).unwrap();
    }
    let txn = doc.transact();
    assert_eq!(model::origin(&txn, "run-1"), Some(origin));
    assert_eq!(model::origin(&txn, "nope"), None);
    assert_eq!(model::event(&txn, EVENT).unwrap().author, None);
}

#[test]
fn columns_and_rule_change() {
    let doc = written();
    let rule = json!({"filter": {"all": true}, "where": {"has": "#q"}});
    {
        let mut txn = doc.transact_mut();
        assert!(model::set_catalogue_columns(
            &mut txn,
            ALPHA,
            &["flag".to_owned()]
        ));
        assert!(model::set_catalogue_rule(&mut txn, ALPHA, Some(&rule)).unwrap());
    }
    let catalogue = model::catalogue(&doc.transact(), ALPHA).unwrap();
    assert_eq!(catalogue.columns, ["flag"]);
    assert_eq!(catalogue.rule, Some(rule));

    model::set_catalogue_rule(&mut doc.transact_mut(), ALPHA, None).unwrap();
    assert_eq!(model::catalogue(&doc.transact(), ALPHA).unwrap().rule, None);
}

#[test]
fn catalogue_tags_change_sorted_and_unique() {
    let doc = written();
    let tags = ["b", "a", "b"].map(str::to_owned);
    assert!(model::set_catalogue_tags(
        &mut doc.transact_mut(),
        ALPHA,
        &tags
    ));
    assert_eq!(
        model::catalogue(&doc.transact(), ALPHA).unwrap().tags,
        ["a", "b"]
    );
    assert!(model::set_catalogue_tags(
        &mut doc.transact_mut(),
        ALPHA,
        &[]
    ));
    assert!(
        model::catalogue(&doc.transact(), ALPHA)
            .unwrap()
            .tags
            .is_empty()
    );
}

#[test]
fn existing_tags_cover_live_events_and_catalogues() {
    let doc = written();
    {
        let mut txn = doc.transact_mut();
        let mut gone = hand_made(OTHER);
        gone.tags = vec!["hidden".to_owned()];
        model::create_event(&mut txn, &gone).unwrap();
        model::delete_event(&mut txn, OTHER);
    }
    assert_eq!(model::existing_tags(&doc.transact()), ["c1", "t1", "t2"]);
}

#[test]
fn ranges_names_and_members_change() {
    let doc = written();
    let (start, stop) = (at("2025-02-01 00:00:00.000001"), at("2025-02-01 00:01:00"));
    {
        let mut txn = doc.transact_mut();
        assert!(model::rename_catalogue(&mut txn, ALPHA, "renamed"));
        assert!(model::set_event_range(&mut txn, EVENT, start, stop).unwrap());
        assert!(model::remove_catalogue_events(
            &mut txn,
            ALPHA,
            &[EVENT.to_owned()]
        ));
    }
    let txn = doc.transact();
    let catalogue = model::catalogue(&txn, ALPHA).unwrap();
    assert_eq!(catalogue.name, "renamed");
    assert_eq!(catalogue.event_count, 0);
    let event = model::event(&txn, EVENT).unwrap();
    assert_eq!((event.start, event.stop), (start, stop));
}

#[test]
fn writing_to_an_unknown_object_changes_nothing() {
    let doc = written();
    let empty = Map::new();
    let now = at("2025-01-31 10:00:00");
    {
        let mut txn = doc.transact_mut();
        assert!(!model::rename_catalogue(&mut txn, UNKNOWN, "nothing"));
        assert!(!model::delete_catalogue(&mut txn, UNKNOWN));
        assert!(!model::purge_catalogue(&mut txn, UNKNOWN));
        assert!(!model::delete_event(&mut txn, UNKNOWN));
        assert!(!model::restore_event(&mut txn, UNKNOWN));
        assert!(!model::purge_event(&mut txn, UNKNOWN));
        assert!(!model::set_catalogue_attributes(&mut txn, UNKNOWN, &empty).unwrap());
        assert!(!model::remove_catalogue_attributes(&mut txn, UNKNOWN, &[]));
        assert_eq!(
            model::set_event_attributes(&mut txn, UNKNOWN, &empty).unwrap(),
            None
        );
        assert!(!model::remove_event_attributes(&mut txn, UNKNOWN, &[]));
        assert!(!model::add_catalogue_events(&mut txn, UNKNOWN, &[]));
        assert!(!model::remove_catalogue_events(&mut txn, UNKNOWN, &[]));
        assert!(!model::set_catalogue_columns(&mut txn, UNKNOWN, &[]));
        assert!(!model::set_catalogue_tags(&mut txn, UNKNOWN, &[]));
        assert!(!model::set_event_range(&mut txn, UNKNOWN, now, now).unwrap());
        let status = EventField::Status(Status::Final);
        assert!(!model::set_event_field(&mut txn, UNKNOWN, &status).unwrap());
    }
    let txn = doc.transact();
    assert_eq!(model::catalogues(&txn).len(), 1);
    assert_eq!(txn.get_map(model::EVENTS).unwrap().len(&txn), 1);
}
