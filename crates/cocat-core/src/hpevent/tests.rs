use serde_json::json;
use yrs::{Doc, Transact};

use super::*;
use crate::model;

fn at(text: &str) -> Timestamp {
    Timestamp::parse(text).unwrap()
}

/// From Lan Jian's STEREO shock list: a stray `#` in a field name, quoted strings holding
/// tabs, and trailing tabs.
const SHOCKS: &str = "#EventTableVersion: HPEvent 1.0\r\n\
#ListTitle: List of Shocks Observed at STEREO A/B\r\n\
#ListID: spase://VSPO/Catalog/LanJian_STEREO_Shock_List\r\n\
#FieldNames: shock_start_time shock_end_time STEREO magnetic_field_increase_ratio shock_normal_angle beta #magnetosonic_mach_number availability_of_burst_mode_data shock_source comment\r\n\
#FieldUnits: DateTime DateTime None None degree None None None None None\r\n\
#FieldNulls: 0001-01-01T00:00:00.000Z 0001-01-01T00:00:00.000Z 0 -1 -1 -1 -1 0 None None\r\n\
#Contact: Lan Jian\r\n\
#Description:\r\n\
#The event list is compiled by Dr. Lan Jian for reference purpose.\r\n\
#In the comment, F for forward shock, R for reverse shock\r\n\
#\r\n\
2007-01-14T19:35:08.000Z\t2007-01-14T19:35:08.000Z\t1\t1.19\t67.6\t-1.00\t1.1\t1\t\"ICME\"\t\"F\"\r\n\
2014-09-25T20:38:51.000Z\t2014-09-25T20:38:51.000Z\t1\t1.59\t39.09\t0.21\t1.50\t-1\t\"\tfast wind overtake an ICME\t\"\t\"\tF, large upstream waves\t\"\t\t\t\t\n";

/// A list from CDAWeb's ISTP catalog: space-separated, `NULL` cells, `_` for spaces.
const CLOUDS: &str = "# ListTitle: Interplanetary magnetic clouds \n\
# FieldNames: TIME_START TIME_END CATEGORY S_C COMMENTS\n\
# FieldUnits: DateTime DateTime text text text\n\
# FieldNulls: 0001-01-01T00:00:00.0Z 0001-01-01T00:00:00.0Z NULL NULL NULL\n\
1994-03-03T08:30:00.000Z 1994-03-03T19:00:00.000Z IMC 8 Likely_start_earlier\n\
1994-12-05T21:00:00.000Z 0001-01-01T00:00:00.0Z IS W NULL\n";

#[test]
fn a_tabbed_list_reads_its_quoted_cells_and_skips_its_nulls() {
    let list = read(SHOCKS, None).unwrap();
    assert_eq!(
        list.title.as_deref(),
        Some("List of Shocks Observed at STEREO A/B")
    );
    assert_eq!(
        list.list_id.as_deref(),
        Some("spase://VSPO/Catalog/LanJian_STEREO_Shock_List")
    );
    assert_eq!(list.contact.as_deref(), Some("Lan Jian"));
    assert_eq!(
        list.description.as_deref(),
        Some(
            "The event list is compiled by Dr. Lan Jian for reference purpose.\nIn the comment, F for forward shock, R for reverse shock"
        )
    );
    let names: Vec<&str> = list
        .fields
        .iter()
        .map(|field| field.name.as_str())
        .collect();
    assert_eq!(names[4], "magnetosonic_mach_number", "the stray # goes");
    assert_eq!(list.fields[2].unit.as_deref(), Some("degree"));
    assert_eq!(list.fields[0].unit, None, "None is no unit");
    assert_eq!(list.events.len(), 2);
    let first = &list.events[0];
    assert_eq!(
        (first.start, first.stop),
        (at("2007-01-14T19:35:08Z"), at("2007-01-14T19:35:08Z"))
    );
    assert_eq!(first.attributes["shock_normal_angle"], json!(67.6));
    assert!(
        !first.attributes.contains_key("beta"),
        "-1.00 is the null of beta"
    );
    assert_eq!(first.attributes["comment"], json!("F"));
    let second = &list.events[1];
    assert_eq!(
        second.attributes["shock_source"],
        json!("fast wind overtake an ICME")
    );
    assert_eq!(
        second.attributes["comment"],
        json!("F, large upstream waves")
    );
}

#[test]
fn a_spaced_list_makes_a_null_stop_a_point() {
    let list = read(CLOUDS, None).unwrap();
    assert_eq!(
        list.title.as_deref(),
        Some("Interplanetary magnetic clouds")
    );
    assert_eq!(list.events.len(), 2);
    assert_eq!(
        list.events[0].attributes["COMMENTS"],
        json!("Likely_start_earlier")
    );
    assert_eq!(list.events[0].attributes["S_C"], json!(8));
    let point = &list.events[1];
    assert_eq!(point.start, point.stop);
    assert!(
        !point.attributes.contains_key("COMMENTS"),
        "NULL is the null"
    );
}

#[test]
fn names_given_replace_a_header_that_miscounts_them() {
    let sirs = "#FieldNames: SIR_start_time SIR_end time Vmax\n#FieldUnits: DateTime DateTime km/s\n\
2007-02-25T17:40:00.000Z\t2007-03-01T01:10:00.000Z\t693\n";
    let list = read(sirs, Some(&["SIR_start_time", "SIR_end_time", "Vmax"])).unwrap();
    assert_eq!(list.events[0].attributes["Vmax"], json!(693));
    assert_eq!(list.fields[0].unit.as_deref(), Some("km/s"));
}

#[test]
fn a_list_without_field_names_or_with_a_bad_time_is_refused() {
    assert!(matches!(
        read("2007-01-14T19:35:08Z\n", None),
        Err(HpEventError::NoFieldNames)
    ));
    let bad = "#FieldNames: start stop\nyesterday 2007-01-14T19:35:08Z\n";
    assert!(matches!(
        read(bad, None),
        Err(HpEventError::Row { row: 2, .. })
    ));
}

#[test]
fn an_import_keeps_the_units_and_the_description() {
    let import = parse(SHOCKS, "shocks.txt").unwrap();
    assert_eq!(import.name, "List of Shocks Observed at STEREO A/B");
    let doc = Doc::new();
    let written = write(&mut doc.transact_mut(), &import, None).unwrap();
    let txn = doc.transact();
    let catalogue = model::catalogue(&txn, &written.catalogues[0]).unwrap();
    assert_eq!(catalogue.event_count, 2);
    assert!(
        catalogue.attributes["description"]
            .as_str()
            .unwrap()
            .starts_with("The event list")
    );
    let types = model::attribute_types(&txn);
    assert_eq!(types["shock_normal_angle"].unit.as_deref(), Some("degree"));
}

#[test]
fn an_unnamed_list_takes_its_file_name() {
    assert_eq!(
        parse("#FieldNames: start stop\n", "my_list.txt")
            .unwrap()
            .name,
        "my_list"
    );
}

#[test]
fn an_export_reads_back_as_the_same_events() {
    let import = parse(SHOCKS, "shocks.txt").unwrap();
    let doc = Doc::new();
    let written = write(&mut doc.transact_mut(), &import, None).unwrap();
    let txn = doc.transact();
    let members = model::catalogue_members(&txn, &written.catalogues[0])
        .unwrap()
        .unwrap();
    let text = export(&txn, members.iter(), "STEREO shocks");
    assert!(text.starts_with("#EventTableVersion: HPEvent"));
    assert!(text.contains("#ListTitle: STEREO shocks"));
    let again = read(&text, None).unwrap();
    assert_eq!(again.events.len(), 2);
    assert_eq!(
        again.events[1].attributes["comment"],
        json!("F, large upstream waves")
    );
    assert_eq!(
        again.events[0].attributes["shock_normal_angle"],
        json!(67.6)
    );
    let units: Vec<_> = again
        .fields
        .iter()
        .map(|field| (field.name.as_str(), field.unit.as_deref()))
        .collect();
    assert!(units.contains(&("shock_normal_angle", Some("degree"))));
}
