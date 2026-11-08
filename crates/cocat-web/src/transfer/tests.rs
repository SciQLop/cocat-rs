use serde_json::Value;
use yrs::{Doc, Transact};

use super::{Format, Incoming, Summary, TransferError, export, file_name, read, summary, write};
use crate::fixture::{DAY0, at, room};
use cocat_core::model;

fn exported(doc: &Doc, uuids: &[&str], format: Format) -> String {
    let uuids: Vec<String> = uuids.iter().map(|uuid| (*uuid).to_owned()).collect();
    export(&doc.transact(), &uuids, format).unwrap()
}

/// The room, with no attributes: a VOTable needs every event to carry the same ones.
fn uniform() -> Doc {
    let doc = room();
    let keys = ["amplitude", "region", "spacecraft"].map(str::to_owned);
    for uuid in ["e1", "e2", "e3"] {
        model::remove_event_attributes(&mut doc.transact_mut(), uuid, &keys);
    }
    doc
}

fn csv_rows(text: &str) -> Vec<Vec<String>> {
    csv::Reader::from_reader(text.as_bytes())
        .records()
        .map(|record| record.unwrap().iter().map(str::to_owned).collect())
        .collect()
}

#[test]
fn the_csv_has_one_row_per_event_and_a_column_per_attribute() {
    let text = exported(&room(), &["c-smart"], Format::Csv);
    let header: Vec<&str> = text.lines().next().unwrap().split(',').collect();
    assert_eq!(
        header,
        [
            "start",
            "stop",
            "tags",
            "status",
            "confidence",
            "author",
            "comment",
            "amplitude",
            "region",
            "spacecraft"
        ]
    );
    let rows = csv_rows(&text);
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows[0],
        [
            "2019-11-03T14:02:11.000000Z",
            "2019-11-03T14:02:52.000000Z",
            "sheath;sw",
            "confirmed",
            "0.82",
            "alexis.jeandet",
            "",
            "2.4",
            "",
            "MMS1"
        ]
    );
    assert_eq!(rows[1][3], "preliminary");
    assert_eq!(rows[1][5], "");
    assert_eq!(rows[1][6], "Seen by the finder only");
}

#[test]
fn the_csv_quotes_what_needs_it_and_keeps_derived_intervals() {
    let doc = room();
    {
        let mut txn = doc.transact_mut();
        let mut attributes = serde_json::Map::new();
        attributes.insert("note".into(), Value::from("a, \"b\"\nc"));
        model::set_event_attributes(&mut txn, "e1", &attributes).unwrap();
    }
    let rows = csv_rows(&exported(&doc, &["c-static"], Format::Csv));
    assert_eq!(rows[0][8], "a, \"b\"\nc");
    let merged = csv_rows(&exported(&doc, &["c-merged"], Format::Csv));
    let members = model::catalogue_members(&doc.transact(), "c-merged")
        .unwrap()
        .unwrap();
    assert_eq!(merged.len(), members.len());
    let derived = members
        .iter()
        .zip(&merged)
        .filter_map(|(member, row)| match member {
            model::Member::Derived { start, .. } => Some((start, row)),
            model::Member::Event { .. } => None,
        });
    let mut seen = 0;
    for (start, row) in derived {
        assert_eq!(row[0], start.to_string());
        assert!(row[2..].iter().all(String::is_empty), "{row:?}");
        seen += 1;
    }
    assert!(seen > 0);
}

#[test]
fn several_catalogues_go_in_one_votable_or_tscat_file() {
    let doc = uniform();
    let xml = exported(&doc, &["c-static", "c-smart"], Format::Votable);
    let Incoming::Votable(imported) = read(&xml).unwrap() else {
        panic!("not read as a VOTable");
    };
    assert_eq!(imported.tables.len(), 2);
    let json = exported(&doc, &["c-static", "c-smart"], Format::TscatJson);
    let Incoming::Tscat(import) = read(&json).unwrap() else {
        panic!("not read as tscat JSON");
    };
    let names: Vec<&str> = import.catalogues.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Bow shock crossings", "Solitary waves"]);
    assert_eq!(import.events.len(), 3);
}

#[test]
fn a_votable_refuses_events_with_different_attributes() {
    let uuids = ["c-static".to_owned()];
    let refused = export(&room().transact(), &uuids, Format::Votable);
    assert!(matches!(refused, Err(TransferError::Votable(_))));
}

#[test]
fn a_file_is_read_by_what_it_holds() {
    assert!(matches!(
        read("\u{feff}  {\"catalogues\": [], \"events\": []}"),
        Ok(Incoming::Tscat(_))
    ));
    assert!(matches!(
        read("\n<VOTABLE></VOTABLE>"),
        Ok(Incoming::Votable(_))
    ));
    assert!(matches!(read("start,stop\n"), Err(TransferError::Unknown)));
    assert!(matches!(
        read("{\"events\": []}"),
        Err(TransferError::Tscat(_))
    ));
}

#[test]
fn the_summary_counts_what_is_new_and_what_is_updated() {
    let doc = uniform();
    let json = exported(&doc, &["c-static"], Format::TscatJson);
    let incoming = read(&json).unwrap();
    assert_eq!(
        summary(&doc.transact(), &incoming),
        Summary {
            new_catalogues: 0,
            updated_catalogues: 1,
            new_events: 0,
            updated_events: 2,
        }
    );
    let empty = Doc::new();
    let fresh = summary(&empty.transact(), &incoming);
    assert_eq!((fresh.new_catalogues, fresh.new_events), (1, 2));
    let xml = exported(&doc, &["c-static"], Format::Votable);
    let votable = summary(&doc.transact(), &read(&xml).unwrap());
    assert_eq!((votable.new_catalogues, votable.updated_events), (1, 2));
}

#[test]
fn the_summary_says_it_in_one_sentence() {
    let summary = |new_catalogues, updated_catalogues, new_events, updated_events| Summary {
        new_catalogues,
        updated_catalogues,
        new_events,
        updated_events,
    };
    assert_eq!(
        summary(2, 0, 300, 5).text(),
        "Adds 2 catalogues and 300 events, updates 5 events (same ids)."
    );
    assert_eq!(summary(1, 0, 1, 0).text(), "Adds 1 catalogue and 1 event.");
    assert_eq!(
        summary(0, 1, 0, 2).text(),
        "Updates 1 catalogue and 2 events (same ids)."
    );
    assert_eq!(
        summary(0, 0, 0, 0).text(),
        "The file holds no catalogue and no event."
    );
}

#[test]
fn an_export_written_back_restores_the_same_events() {
    let doc = room();
    let json = exported(&doc, &["c-static"], Format::TscatJson);
    let before = model::catalogue_events(&doc.transact(), "c-static")
        .unwrap()
        .unwrap();
    model::purge_catalogue(&mut doc.transact_mut(), "c-static");
    for event in &before {
        model::purge_event(&mut doc.transact_mut(), &event.uuid);
    }
    let written = write(&mut doc.transact_mut(), &read(&json).unwrap(), at(DAY0)).unwrap();
    assert_eq!(written.catalogues, ["c-static"]);
    let txn = doc.transact();
    let after = model::catalogue_events(&txn, "c-static").unwrap().unwrap();
    let uuids = |events: &[model::EventRecord]| -> Vec<String> {
        events.iter().map(|event| event.uuid.clone()).collect()
    };
    assert_eq!(uuids(&after), uuids(&before));
    assert_eq!(after[0].start, before[0].start);
    assert_eq!(after[0].tags, before[0].tags);
}

#[test]
fn a_file_name_is_safe_everywhere() {
    let name = |name: &str| file_name(&[name.to_owned()], Format::Csv);
    assert_eq!(name("Bow shock crossings"), "Bow_shock_crossings.csv");
    assert_eq!(name("a/b\\c:d*?\"<>|"), "a_b_c_d.csv");
    assert_eq!(name("Ondes de choc é"), "Ondes_de_choc_é.csv");
    assert_eq!(name("../.."), "catalogues.csv");
    assert_eq!(name("   "), "catalogues.csv");
    let two = ["a".to_owned(), "b".to_owned()];
    assert_eq!(file_name(&two, Format::Votable), "catalogues.xml");
    assert_eq!(file_name(&two, Format::TscatJson), "catalogues.json");
}
