use serde_json::Value;
use yrs::{Doc, Transact};

use super::{CsvError, export, parse, write};
use crate::model::{self, Status};

const SPREADSHEET: &str = "\u{feff}Start;End;Tags;Status;Confidence;Comment;density;MMS;label
2020-01-01 10:00:00;2020-01-01 10:05:00;shock, foot;Reviewed;0.9;sharp jump;12.5;true;bow
2020-01-02T00:00:00Z;2020-01-02T01:00:00Z;;maybe;2;;7;false;
";

#[test]
fn a_spreadsheet_reads_into_events_with_fields_and_typed_attributes() {
    let import = parse(SPREADSHEET, "data/shocks 2020.csv").unwrap();

    assert_eq!(import.name, "shocks 2020");
    assert_eq!(import.columns, ["density", "MMS", "label"]);
    let first = &import.events[0];
    assert_eq!(first.start.to_string(), "2020-01-01T10:00:00.000000Z");
    assert_eq!(first.stop.to_string(), "2020-01-01T10:05:00.000000Z");
    assert_eq!(first.tags, ["shock", "foot"]);
    assert_eq!(first.status, Status::Reviewed);
    assert_eq!(first.confidence, Some(0.9));
    assert_eq!(first.attributes["density"], Value::from(12.5));
    assert_eq!(first.attributes["MMS"], Value::Bool(true));
    assert_eq!(first.attributes["label"], Value::from("bow"));
    assert_eq!(
        import.comments,
        [(first.uuid.clone(), "sharp jump".to_owned())]
    );

    let second = &import.events[1];
    assert_eq!(second.attributes["density"], Value::from(7));
    assert!(!second.attributes.contains_key("label"));
    assert_eq!(second.confidence, None);
    assert_eq!(import.warnings.len(), 2, "{:?}", import.warnings);
    assert!(import.warnings[0].starts_with("Row 3: the status “maybe”"));
}

#[test]
fn a_tab_separated_file_reads_too() {
    let import = parse("start\tstop\tx\n2020-01-01\t2020-01-02\t1\n", "t.tsv").unwrap();
    assert_eq!(import.events.len(), 1);
    assert_eq!(import.events[0].attributes["x"], Value::from(1));
}

#[test]
fn a_file_without_start_or_stop_says_which_is_missing() {
    let error = parse("begin,duration\n2020-01-01,3\n", "f.csv").unwrap_err();
    assert!(matches!(error, CsvError::Missing("stop")));
}

#[test]
fn a_bad_time_names_its_row_and_column() {
    let error = parse(
        "start,stop\n2020-01-01,2020-01-02\n2020-01-01,soon\n",
        "f.csv",
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Row 3: the stop “soon” is not a time, such as 2020-01-31T10:00:00Z."
    );
}

#[test]
fn an_export_reads_back_as_the_same_events() {
    let doc = Doc::new();
    let import = parse(SPREADSHEET, "shocks.csv").unwrap();
    let written = write(&mut doc.transact_mut(), &import, None).unwrap();
    let catalogue = &written.catalogues[0];
    let txn = doc.transact();
    assert_eq!(model::catalogue(&txn, catalogue).unwrap().name, "shocks");
    let first = &import.events[0].uuid;
    assert_eq!(model::comment(&txn, first).as_deref(), Some("sharp jump"));

    let members = model::catalogue_members(&txn, catalogue).unwrap().unwrap();
    let text = export(&txn, members.iter()).unwrap();
    drop(txn);
    let again = parse(&text, "shocks.csv").unwrap();

    assert_eq!(again.events.len(), 2);
    for event in &again.events {
        let stored = model::event(&doc.transact(), &event.uuid).unwrap();
        assert_eq!(event.start, stored.start);
        assert_eq!(event.tags, stored.tags);
        assert_eq!(event.attributes, stored.attributes);
    }
    assert!(
        again
            .comments
            .contains(&(first.clone(), "sharp jump".to_owned()))
    );
}

#[test]
fn importing_an_export_again_updates_its_events() {
    let doc = Doc::new();
    let import = parse(SPREADSHEET, "shocks.csv").unwrap();
    let written = write(&mut doc.transact_mut(), &import, None).unwrap();
    let text = {
        let txn = doc.transact();
        let members = model::catalogue_members(&txn, &written.catalogues[0])
            .unwrap()
            .unwrap();
        export(&txn, members.iter()).unwrap()
    };
    let edited = text.replace(",12.5,", ",13,");
    write(
        &mut doc.transact_mut(),
        &parse(&edited, "shocks.csv").unwrap(),
        Some("v2"),
    )
    .unwrap();

    let txn = doc.transact();
    let stored = model::event(&txn, &import.events[0].uuid).unwrap();
    assert_eq!(stored.attributes["density"], Value::from(13));
    assert_eq!(model::catalogues(&txn).len(), 2);
}
