use serde_json::{Value, json};
use yrs::{Doc, Transact};

use super::{ORIGIN, TscatJsonError, export, parse, write};
use crate::model::{self, AttributeType, EventField, ShapeAxis, Status, Vertex};
use crate::time::Timestamp;

const ALICE: &str = "00000000-0000-4000-8000-00000000000a";
const BOB: &str = "00000000-0000-4000-8000-00000000000b";
const LISTED: &str = "00000000-0000-4000-8000-0000000000c1";
const SMART: &str = "00000000-0000-4000-8000-0000000000c2";

fn time(text: &str) -> Timestamp {
    Timestamp::parse(text).unwrap()
}

fn now() -> Timestamp {
    time("2026-01-01 00:00:00")
}

/// A file as tscat's `export_json` writes it: one listed catalogue, and a dynamic one whose
/// predicate tscat wrote as its repr.
fn file() -> Value {
    json!({
        "catalogues": [
            {"name": "listed", "author": "alice", "uuid": LISTED, "tags": ["t"],
             "predicate": null, "k": 1, "events": [ALICE, BOB]},
            {"name": "smart", "author": "bob", "uuid": SMART, "tags": [],
             "predicate": "Comparison('>', Field('rating'), 5)", "events": [ALICE]},
        ],
        "events": [
            {"start": "2020-01-01T00:00:00", "stop": "2020-01-01T01:00:00", "author": "alice",
             "uuid": ALICE, "tags": ["a", "b"], "products": ["speasy/x"], "rating": 7,
             "n": 3, "flag": true, "when": "2020-01-02T00:00:00"},
            {"start": "2020-01-01T02:00:00.250000", "stop": "2020-01-01T03:00:00",
             "author": "", "uuid": BOB, "tags": [], "products": [], "rating": null},
        ],
    })
}

fn written(file: &Value) -> Doc {
    let doc = Doc::new();
    let import = parse(&file.to_string()).unwrap();
    write(&mut doc.transact_mut(), &import, now()).unwrap();
    doc
}

#[test]
fn an_event_keeps_its_uuid_and_its_rating_becomes_a_confidence() {
    let import = parse(&file().to_string()).unwrap();
    let alice = &import.events[0];
    assert_eq!(alice.uuid, ALICE);
    assert_eq!(alice.start, time("2020-01-01 00:00:00"));
    assert_eq!(alice.author.as_deref(), Some("alice"));
    assert_eq!(alice.confidence, Some(0.7));
    assert_eq!(alice.origin.as_deref(), Some(ORIGIN));
    assert_eq!(alice.tags, ["a", "b"]);
    assert_eq!(alice.products, ["speasy/x"]);
    assert_eq!(alice.attributes["n"], json!(3));
    assert_eq!(alice.attributes["flag"], json!(true));
    let bob = &import.events[1];
    assert_eq!(bob.author, None);
    assert_eq!(bob.confidence, None);
    assert_eq!(bob.start, time("2020-01-01 02:00:00.25"));
}

#[test]
fn tscats_datetime_text_comes_back_as_a_date() {
    let import = parse(&file().to_string()).unwrap();
    assert_eq!(
        import.events[0].attributes["when"],
        json!(time("2020-01-02 00:00:00").to_string())
    );
    assert!(import.dates.contains("when"));
    assert_eq!(import.dates.len(), 1);
}

#[test]
fn a_predicate_written_as_text_keeps_the_members_and_warns() {
    let import = parse(&file().to_string()).unwrap();
    let smart = &import.catalogues[1];
    assert_eq!(smart.rule, None);
    assert_eq!(smart.events, [ALICE]);
    assert_eq!(import.warnings.len(), 1);
    assert!(import.warnings[0].starts_with("catalogue smart:"));
}

#[test]
fn a_predicate_dict_becomes_a_rule() {
    let mut file = file();
    file["catalogues"][1]["predicate"] = json!({"type": "Comparison", "op": ">",
        "lhs": {"type": "Field", "name": "rating"}, "rhs": 5});
    let import = parse(&file.to_string()).unwrap();
    assert!(import.catalogues[1].rule.is_some());
    assert!(import.warnings.is_empty());
}

#[test]
fn a_predicate_that_does_not_convert_lists_its_current_members() {
    let mut file = file();
    file["catalogues"][1]["predicate"] = json!({"type": "Nonsense"});
    file["members"] = json!({SMART: [BOB]});
    let import = parse(&file.to_string()).unwrap();
    assert_eq!(import.catalogues[1].rule, None);
    assert_eq!(import.catalogues[1].events, [BOB]);
    assert!(import.warnings[0].contains("does not convert"));
}

#[test]
fn removed_items_go_to_the_trash() {
    let mut file = file();
    file["removed"] = json!([BOB, SMART]);
    let doc = written(&file);
    let txn = doc.transact();
    assert_eq!(model::event(&txn, BOB).unwrap().status, Status::Deleted);
    assert_eq!(model::event(&txn, ALICE).unwrap().status, Status::Confirmed);
    assert!(model::catalogue(&txn, SMART).unwrap().deleted);
}

#[test]
fn a_bad_time_says_which_event() {
    let mut file = file();
    file["events"][0]["start"] = json!("yesterday");
    let error = parse(&file.to_string()).unwrap_err();
    assert!(matches!(error, TscatJsonError::Time(ref uuid, "start", _) if uuid == ALICE));
    assert!(matches!(
        parse("{\"catalogues\": []}"),
        Err(TscatJsonError::Format(_))
    ));
}

#[test]
fn writing_creates_the_origin_events_and_catalogues() {
    let doc = written(&file());
    let txn = doc.transact();
    assert!(model::origin(&txn, ORIGIN).is_some());
    assert_eq!(
        model::attribute_types(&txn)["when"].kind,
        AttributeType::Date
    );
    let listed = model::catalogue(&txn, LISTED).unwrap();
    assert_eq!(listed.name, "listed");
    assert_eq!(listed.author.as_deref(), Some("alice"));
    assert_eq!(listed.attributes["k"], json!(1));
    assert_eq!(listed.created, Some(now()));
    assert_eq!(model::listed_events(&txn, LISTED), [ALICE, BOB]);
    assert_eq!(model::event(&txn, ALICE).unwrap().confidence, Some(0.7));
}

#[test]
fn importing_again_updates_and_keeps_what_tscat_does_not_know() {
    let doc = written(&file());
    {
        let mut txn = doc.transact_mut();
        model::set_event_field(&mut txn, ALICE, &EventField::Status(Status::Reviewed)).unwrap();
        model::set_event_field(&mut txn, ALICE, &EventField::StartErr(Some(2.0))).unwrap();
        model::delete_event(&mut txn, BOB);
        model::delete_catalogue(&mut txn, LISTED);
    }
    let mut file = file();
    file["events"][0]["rating"] = json!(2);
    file["events"][0]["tags"] = json!(["z"]);
    file["events"][0].as_object_mut().unwrap().remove("n");
    file["catalogues"][0]["name"] = json!("renamed");
    file["catalogues"][0]["events"] = json!([BOB]);
    let import = parse(&file.to_string()).unwrap();
    let written = write(&mut doc.transact_mut(), &import, now()).unwrap();

    assert_eq!(written.catalogues, [LISTED, SMART]);
    let txn = doc.transact();
    let alice = model::event(&txn, ALICE).unwrap();
    assert_eq!(alice.confidence, Some(0.2));
    assert_eq!(alice.tags, ["z"]);
    assert!(!alice.attributes.contains_key("n"));
    assert_eq!(alice.status, Status::Reviewed);
    assert_eq!(alice.start_err, Some(2.0));
    assert_eq!(model::event(&txn, BOB).unwrap().status, Status::Confirmed);
    let listed = model::catalogue(&txn, LISTED).unwrap();
    assert_eq!(listed.name, "renamed");
    assert!(!listed.deleted);
    assert_eq!(model::listed_events(&txn, LISTED), [BOB]);
}

/// The JSON tscat's `import_json` reads: naive times, a whole rating, dates as isoformat.
#[test]
fn the_export_is_tscats_json() {
    let doc = written(&file());
    {
        let mut txn = doc.transact_mut();
        model::set_event_field(&mut txn, BOB, &EventField::Confidence(Some(0.25))).unwrap();
    }
    let txn = doc.transact();
    let listed = model::catalogue(&txn, LISTED).unwrap();
    let events = model::catalogue_events(&txn, LISTED).unwrap().unwrap();
    let text = export(&txn, &[(&listed, &events), (&listed, &events)]).unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();

    assert_eq!(json["events"].as_array().unwrap().len(), 2);
    let alice = &json["events"][0];
    assert_eq!(alice["start"], "2020-01-01T00:00:00");
    assert_eq!(alice["rating"], 7);
    assert_eq!(alice["when"], "2020-01-02T00:00:00+00:00");
    assert_eq!(alice["n"], 3);
    let bob = &json["events"][1];
    assert_eq!(bob["start"], "2020-01-01T02:00:00.250000");
    assert_eq!(bob["author"], "");
    assert_eq!(bob["rating"], 2);
    let catalogue = &json["catalogues"][0];
    assert_eq!(catalogue["predicate"], Value::Null);
    assert_eq!(catalogue["k"], 1);
    assert_eq!(catalogue["events"], json!([ALICE, BOB]));
}

#[test]
fn an_export_imports_back_the_same() {
    let doc = written(&file());
    let txn = doc.transact();
    let listed = model::catalogue(&txn, LISTED).unwrap();
    let events = model::catalogue_events(&txn, LISTED).unwrap().unwrap();
    let text = export(&txn, &[(&listed, &events)]).unwrap();
    let again = parse(&text).unwrap();
    assert_eq!(again.events, events);
    assert_eq!(again.catalogues[0].events, [ALICE, BOB]);
    assert_eq!(again.catalogues[0].name, "listed");
}

fn pitch_angle() -> ShapeAxis {
    ShapeAxis {
        quantity: "pitch angle".to_owned(),
        unit: "deg".to_owned(),
    }
}

fn a_shape() -> Vec<Vertex> {
    let vertex = |text: &str, value: f64| Vertex {
        time: time(text),
        value,
    };
    vec![
        vertex("2020-01-01T00:00:00Z", 10.0),
        vertex("2020-01-01T00:30:00.123456789Z", 0.1 + 0.2),
        vertex("2020-01-01T01:00:00Z", 170.5),
    ]
}

/// Alice has a shape and the listed catalogue a shape axis.
fn shaped() -> Doc {
    let doc = written(&file());
    {
        let mut txn = doc.transact_mut();
        model::set_event_field(&mut txn, ALICE, &EventField::Shape(Some(a_shape()))).unwrap();
        model::set_catalogue_shape_axis(&mut txn, LISTED, Some(&pitch_angle())).unwrap();
    }
    doc
}

fn exported_listed(doc: &Doc) -> String {
    let txn = doc.transact();
    let listed = model::catalogue(&txn, LISTED).unwrap();
    let events = model::catalogue_events(&txn, LISTED).unwrap().unwrap();
    export(&txn, &[(&listed, &events)]).unwrap()
}

#[test]
fn shapes_and_the_shape_axis_are_plain_json() {
    let json: Value = serde_json::from_str(&exported_listed(&shaped())).unwrap();

    assert_eq!(
        json["events"][0]["shape"],
        json!([
            ["2020-01-01T00:00:00Z", 10.0],
            ["2020-01-01T00:30:00.123456789Z", 0.1 + 0.2],
            ["2020-01-01T01:00:00Z", 170.5]
        ])
    );
    assert!(json["events"][1].get("shape").is_none());
    assert_eq!(
        json["catalogues"][0]["shape_axis"],
        json!({"quantity": "pitch angle", "unit": "deg"})
    );
}

#[test]
fn shapes_and_the_shape_axis_import_back() {
    let text = exported_listed(&shaped());
    let doc = Doc::new();
    write(&mut doc.transact_mut(), &parse(&text).unwrap(), now()).unwrap();

    let txn = doc.transact();
    assert_eq!(model::event(&txn, ALICE).unwrap().shape, Some(a_shape()));
    assert_eq!(model::event(&txn, BOB).unwrap().shape, None);
    assert_eq!(
        model::catalogue(&txn, LISTED).unwrap().shape_axis,
        Some(pitch_angle())
    );
    assert!(
        !model::event(&txn, ALICE)
            .unwrap()
            .attributes
            .contains_key("shape")
    );
}

#[test]
fn a_file_without_shapes_leaves_the_projects_shapes() {
    let doc = shaped();
    write(
        &mut doc.transact_mut(),
        &parse(&file().to_string()).unwrap(),
        now(),
    )
    .unwrap();

    let txn = doc.transact();
    assert_eq!(model::event(&txn, ALICE).unwrap().shape, Some(a_shape()));
    assert_eq!(
        model::catalogue(&txn, LISTED).unwrap().shape_axis,
        Some(pitch_angle())
    );
}

#[test]
fn a_shape_with_a_bad_time_says_which_event() {
    let mut file = file();
    file["events"][0]["shape"] = json!([["soon", 1.0]]);
    let error = parse(&file.to_string()).unwrap_err();
    assert!(
        matches!(error, TscatJsonError::Time(ref uuid, "shape", ref text) if uuid == ALICE && text == "soon")
    );
}
