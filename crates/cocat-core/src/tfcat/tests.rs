use serde_json::{Map, Value, json};
use yrs::{Doc, Transact};

use super::{
    CRS, FIELDS, GEOMETRY, TfcatError, TfcatImport, export, needs_value_range, parse,
    strictness_warning, write,
};
use crate::model::{self, CatalogueSummary, EventRecord, NewCatalogue, ShapeAxis, Vertex};
use crate::time::Timestamp;

const REAL: &str =
    include_str!("../../tests/fixtures/tfcat/cassini_faraday_patches_2006_excerpt.json");

fn time(text: &str) -> Timestamp {
    Timestamp::parse(text).unwrap()
}

fn now() -> Timestamp {
    time("2026-01-01 00:00:00")
}

fn unix_crs() -> Value {
    json!({"type": "local", "properties": {"time_coords_id": "unix",
        "spectral_coords": {"type": "frequency", "unit": "kHz"},
        "ref_position_id": "cassini-orbiter"}})
}

fn collection(features: Vec<Value>) -> String {
    json!({
        "type": "FeatureCollection",
        "features": features,
        "properties": {"title": "Patches", "instrument_name": "RPWS"},
        "fields": {"quality": {"info": "Quality", "datatype": "int", "ucd": "meta.code.qual"}},
        "crs": unix_crs(),
    })
    .to_string()
}

fn feature(id: Value, geometry: Value) -> Value {
    json!({"type": "Feature", "id": id, "geometry": geometry, "properties": {"quality": 2}})
}

fn square() -> Value {
    json!({"type": "Polygon", "coordinates": [[[10.0, 1.0], [20.0, 1.0], [20.0, 2.0], [10.0, 2.0], [10.0, 1.0]]]})
}

fn unix(seconds: i64) -> Timestamp {
    Timestamp::from_unix_nanos(seconds * 1_000_000_000).unwrap()
}

/// The import written into a fresh project, read back as the catalogue and its events.
fn stored(import: &TfcatImport) -> (Doc, CatalogueSummary, Vec<EventRecord>) {
    let doc = Doc::new();
    let written = {
        let mut txn = doc.transact_mut();
        write(&mut txn, import, "imported", now()).unwrap()
    };
    let txn = doc.transact();
    let uuid = &written.catalogues[0];
    let catalogue = model::catalogue(&txn, uuid).unwrap();
    let mut events = model::catalogue_events(&txn, uuid).unwrap().unwrap();
    events.sort_by_key(|event| event.start);
    drop(txn);
    (doc, catalogue, events)
}

fn exported(
    doc: &Doc,
    catalogue: &CatalogueSummary,
    events: &[EventRecord],
    range: Option<(f64, f64)>,
) -> Result<String, TfcatError> {
    export(&doc.transact(), catalogue, events, range)
}

/// What an event carries that a TFCat file holds.
fn content(event: &EventRecord) -> Value {
    json!({
        "start": event.start,
        "stop": event.stop,
        "shape": event.shape,
        "source_id": event.source_id,
        "attributes": event.attributes,
    })
}

#[test]
fn a_polygon_becomes_the_events_shape() {
    let import = parse(&collection(vec![feature(json!(7), square())])).unwrap();
    let [event] = import.events.as_slice() else {
        panic!("one event")
    };
    assert_eq!(event.start, unix(10));
    assert_eq!(event.stop, unix(20));
    let corners: Vec<(Timestamp, f64)> = event
        .shape
        .iter()
        .flatten()
        .map(|vertex| (vertex.time, vertex.value))
        .collect();
    assert_eq!(
        corners,
        [
            (unix(10), 1.0),
            (unix(20), 1.0),
            (unix(20), 2.0),
            (unix(10), 2.0)
        ]
    );
    assert_eq!(event.source_id.as_deref(), Some("7"));
    assert_eq!(
        event.attributes,
        Map::from_iter([("quality".to_owned(), json!(2))])
    );
    assert!(import.warnings.is_empty());
}

#[test]
fn the_collection_names_the_catalogue_and_keeps_its_axes_and_fields() {
    let import = parse(&collection(vec![feature(json!("a"), square())])).unwrap();
    assert_eq!(import.title.as_deref(), Some("Patches"));
    assert_eq!(import.attributes["instrument_name"], json!("RPWS"));
    assert!(!import.attributes.contains_key("title"));
    let crs: Value = serde_json::from_str(import.attributes[CRS].as_str().unwrap()).unwrap();
    assert_eq!(crs, unix_crs());
    let fields: Value = serde_json::from_str(import.attributes[FIELDS].as_str().unwrap()).unwrap();
    assert_eq!(fields["quality"]["ucd"], "meta.code.qual");
    assert_eq!(import.name(None, "dir/file.json"), "Patches");
    assert_eq!(import.name(Some("Mine"), "dir/file.json"), "Mine");
    assert_eq!(import.shape_axis, Some(axis("frequency", "kHz")));
}

#[test]
fn without_a_title_the_file_names_the_catalogue() {
    let text = json!({"type": "FeatureCollection", "features": [], "crs": unix_crs()}).to_string();
    let import = parse(&text).unwrap();
    assert_eq!(
        import.name(None, "/data/skr bursts.tfcat.json"),
        "skr bursts.tfcat"
    );
}

#[test]
fn holes_are_dropped_with_one_warning() {
    let holed = json!({"type": "Polygon", "coordinates": [
        [[10.0, 1.0], [20.0, 1.0], [20.0, 2.0], [10.0, 1.0]],
        [[12.0, 1.2], [14.0, 1.2], [14.0, 1.4], [12.0, 1.2]]]});
    let import = parse(&collection(vec![
        feature(json!(1), holed.clone()),
        feature(json!(2), holed),
    ]))
    .unwrap();
    assert_eq!(import.events[0].shape.as_ref().map(Vec::len), Some(3));
    assert_eq!(import.warnings.len(), 1, "{:?}", import.warnings);
}

#[test]
fn every_other_geometry_keeps_its_time_span_and_itself() {
    let geometries = [
        json!({"type": "Point", "coordinates": [15.0, 3.0]}),
        json!({"type": "MultiPoint", "coordinates": [[12.0, 3.0], [18.0, 4.0]]}),
        json!({"type": "LineString", "coordinates": [[11.0, 3.0], [19.0, 4.0]]}),
        json!({"type": "MultiLineString", "coordinates": [[[11.0, 3.0], [13.0, 4.0]], [[16.0, 3.0], [19.0, 4.0]]]}),
        json!({"type": "MultiPolygon", "coordinates": [
            [[[11.0, 1.0], [12.0, 1.0], [12.0, 2.0], [11.0, 1.0]]],
            [[[17.0, 1.0], [19.0, 1.0], [19.0, 2.0], [17.0, 1.0]]]]}),
        json!({"type": "GeometryCollection", "geometries": [
            {"type": "Point", "coordinates": [11.0, 3.0]},
            {"type": "LineString", "coordinates": [[14.0, 3.0], [19.0, 4.0]]}]}),
    ];
    let spans = [(15, 15), (12, 18), (11, 19), (11, 19), (11, 19), (11, 19)];
    for (geometry, (start, stop)) in geometries.into_iter().zip(spans) {
        let import = parse(&collection(vec![feature(json!(1), geometry.clone())])).unwrap();
        let event = &import.events[0];
        assert_eq!(
            (event.start, event.stop),
            (unix(start), unix(stop)),
            "{geometry}"
        );
        assert_eq!(event.shape, None);
        let kept: Value =
            serde_json::from_str(event.attributes[GEOMETRY].as_str().unwrap()).unwrap();
        assert_eq!(kept, geometry);
    }
}

#[test]
fn a_multipolygon_of_one_polygon_is_a_shape() {
    let one = json!({"type": "MultiPolygon", "coordinates": [square()["coordinates"].clone()]});
    let import = parse(&collection(vec![feature(json!(1), one)])).unwrap();
    assert_eq!(import.events[0].shape.as_ref().map(Vec::len), Some(4));
    assert!(!import.events[0].attributes.contains_key(GEOMETRY));
}

#[test]
fn a_feature_without_geometry_is_left_out_with_a_warning() {
    let import = parse(&collection(vec![
        feature(json!(1), Value::Null),
        feature(json!(2), square()),
    ]))
    .unwrap();
    assert_eq!(import.events.len(), 1);
    assert_eq!(import.warnings.len(), 1);
}

#[test]
fn a_file_without_a_coordinate_system_is_refused_in_plain_words() {
    let text = json!({"type": "FeatureCollection", "features": []}).to_string();
    let error = parse(&text).unwrap_err();
    assert!(matches!(error, TfcatError::NoCrs));
    assert!(error.to_string().contains("crs"), "{error}");
}

#[test]
fn other_json_is_not_tfcat() {
    assert!(matches!(
        parse(r#"{"catalogues": [], "events": []}"#),
        Err(TfcatError::Format(_))
    ));
}

#[test]
fn a_round_trip_gives_the_same_events_shapes_attributes_and_axes() {
    let point = json!({"type": "MultiPoint", "coordinates": [[30.5, 3.0], [40.25, 4.0]]});
    let first = parse(&collection(vec![
        feature(json!(7), square()),
        feature(json!("b-2"), point),
    ]))
    .unwrap();
    let (doc, catalogue, events) = stored(&first);
    let text = exported(&doc, &catalogue, &events, None).unwrap();
    let second = parse(&text).unwrap();
    let (_, catalogue_again, events_again) = stored(&second);

    assert_eq!(
        events.iter().map(content).collect::<Vec<_>>(),
        events_again.iter().map(content).collect::<Vec<_>>()
    );
    assert_eq!(second.title.as_deref(), Some("imported"));
    assert_eq!(catalogue.attributes, catalogue_again.attributes);
    assert_eq!(catalogue.shape_axis, Some(axis("frequency", "kHz")));
    assert_eq!(catalogue_again.shape_axis, catalogue.shape_axis);
    let file: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(file["features"][0]["id"], json!(7));
    assert_eq!(file["features"][1]["id"], json!("b-2"));
    assert_eq!(
        file["features"][0]["geometry"]["coordinates"][0]
            .as_array()
            .map(Vec::len),
        Some(5)
    );
    assert!(file["features"][0]["properties"].get(GEOMETRY).is_none());
}

#[test]
fn a_published_catalogue_imports_and_exports_back() {
    let import = parse(REAL).unwrap();
    assert!(
        import
            .title
            .as_deref()
            .unwrap()
            .starts_with("Catalogue of Faraday Rotation (FR) patches")
    );
    assert_eq!(import.attributes["instrument_host_name"], json!("Cassini"));
    assert_eq!(
        import.attributes["authors"][0]["FamilyName"],
        json!("Taubenschuss")
    );
    let ids: Vec<Option<&str>> = import
        .events
        .iter()
        .map(|event| event.source_id.as_deref())
        .collect();
    assert_eq!(ids, [Some("1"), Some("21")]);
    let first = &import.events[0];
    assert_eq!(first.attributes["quality"], json!(3));
    assert!(first.attributes.contains_key(GEOMETRY));
    assert!(first.start < first.stop);
    assert_eq!(first.start.to_string()[..10], *"2006-09-12");

    let (doc, catalogue, events) = stored(&import);
    let file: Value =
        serde_json::from_str(&exported(&doc, &catalogue, &events, None).unwrap()).unwrap();
    let original: Value = serde_json::from_str(REAL).unwrap();
    assert_eq!(file["crs"], original["crs"]);
    assert_eq!(file["fields"], original["fields"]);
    assert_eq!(
        file["features"][0]["geometry"],
        original["features"][0]["geometry"]
    );
}

fn axis(quantity: &str, unit: &str) -> ShapeAxis {
    ShapeAxis {
        quantity: quantity.to_owned(),
        unit: unit.to_owned(),
    }
}

/// A catalogue made by hand: events with neither a shape nor a frequency.
fn plain(events: Vec<EventRecord>) -> (Doc, CatalogueSummary, Vec<EventRecord>) {
    described(events, None)
}

fn described(
    events: Vec<EventRecord>,
    shape_axis: Option<ShapeAxis>,
) -> (Doc, CatalogueSummary, Vec<EventRecord>) {
    let doc = Doc::new();
    let uuid = "00000000-0000-4000-8000-0000000000c1";
    {
        let mut txn = doc.transact_mut();
        for event in &events {
            model::create_event(&mut txn, event).unwrap();
        }
        let catalogue = NewCatalogue {
            uuid: uuid.into(),
            name: "hand made".into(),
            ..NewCatalogue::default()
        };
        model::create_catalogue(&mut txn, &catalogue).unwrap();
        let uuids: Vec<String> = events.iter().map(|event| event.uuid.clone()).collect();
        model::add_catalogue_events(&mut txn, uuid, &uuids);
        model::set_catalogue_shape_axis(&mut txn, uuid, shape_axis.as_ref()).unwrap();
    }
    let catalogue = model::catalogue(&doc.transact(), uuid).unwrap();
    (doc, catalogue, events)
}

#[test]
fn events_without_frequency_ask_for_a_range() {
    let (doc, catalogue, events) = plain(vec![EventRecord::new("e1", unix(100), unix(160))]);
    assert!(needs_value_range(&events));
    let error = exported(&doc, &catalogue, &events, None).unwrap_err();
    assert_eq!(
        error.to_string(),
        "These events have no frequency: give a frequency range"
    );

    let file: Value =
        serde_json::from_str(&exported(&doc, &catalogue, &events, Some((5.0, 9.0))).unwrap())
            .unwrap();
    assert_eq!(
        file["features"][0]["geometry"],
        json!({"type": "Polygon", "coordinates": [[
        [100.0, 5.0], [160.0, 5.0], [160.0, 9.0], [100.0, 9.0], [100.0, 5.0]]]})
    );
    assert_eq!(file["features"][0]["id"], json!("e1"));
    assert_eq!(file["properties"]["title"], json!("hand made"));
    assert_eq!(file["crs"]["properties"]["time_coords_id"], json!("unix"));
    assert_eq!(
        file["crs"]["properties"]["spectral_coords"]["unit"],
        json!("Hz")
    );
    assert!(file["crs"]["properties"]["ref_position_id"].is_string());
}

#[test]
fn shapeless_events_take_the_band_of_their_shaped_neighbours() {
    let mut shaped = EventRecord::new("e1", unix(10), unix(20));
    shaped.shape = Some(vec![
        Vertex {
            time: unix(10),
            value: 3.0,
        },
        Vertex {
            time: unix(20),
            value: 8.0,
        },
        Vertex {
            time: unix(15),
            value: 4.0,
        },
    ]);
    let mut bare = EventRecord::new("e2", unix(30), unix(40));
    bare.attributes.insert("note".into(), json!("faint"));
    let (doc, catalogue, events) = plain(vec![shaped, bare]);
    assert!(!needs_value_range(&events));
    let file: Value =
        serde_json::from_str(&exported(&doc, &catalogue, &events, None).unwrap()).unwrap();
    assert_eq!(
        file["features"][1]["geometry"]["coordinates"][0][0],
        json!([30.0, 3.0])
    );
    assert_eq!(
        file["features"][1]["geometry"]["coordinates"][0][2],
        json!([40.0, 8.0])
    );
    assert_eq!(file["features"][1]["properties"]["note"], json!("faint"));
    assert_eq!(file["fields"]["note"]["datatype"], json!("str"));
    assert!(file["fields"]["note"]["ucd"].is_string());
}

#[test]
fn a_catalogue_by_hand_exports_its_shape_axis_into_the_crs() {
    let events = vec![EventRecord::new("e1", unix(100), unix(160))];
    let (doc, catalogue, events) = described(events, Some(axis("energy", "eV")));
    let error = exported(&doc, &catalogue, &events, None).unwrap_err();
    assert_eq!(
        error.to_string(),
        "These events have no energy: give an energy range"
    );

    let file: Value =
        serde_json::from_str(&exported(&doc, &catalogue, &events, Some((10.0, 30.0))).unwrap())
            .unwrap();
    let properties = &file["crs"]["properties"];
    assert_eq!(
        properties["spectral_coords"],
        json!({"type": "energy", "unit": "eV"})
    );
    assert_eq!(properties["time_coords_id"], json!("unix"));
}

#[test]
fn a_changed_shape_axis_replaces_the_files_spectral_axis() {
    let import = parse(&collection(vec![feature(json!(1), square())])).unwrap();
    let (doc, catalogue, events) = stored(&import);
    let uuid = catalogue.uuid.clone();
    let mhz = axis("frequency", "MHz");
    model::set_catalogue_shape_axis(&mut doc.transact_mut(), &uuid, Some(&mhz)).unwrap();
    let catalogue = model::catalogue(&doc.transact(), &uuid).unwrap();

    let file: Value =
        serde_json::from_str(&exported(&doc, &catalogue, &events, None).unwrap()).unwrap();
    let properties = &file["crs"]["properties"];
    assert_eq!(
        properties["spectral_coords"],
        json!({"type": "frequency", "unit": "MHz"})
    );
    assert_eq!(properties["ref_position_id"], json!("cassini-orbiter"));
}

#[test]
fn an_axis_tfcat_does_not_know_is_exported_with_a_warning() {
    let events = vec![EventRecord::new("e1", unix(100), unix(160))];
    let (doc, catalogue, events) = described(events, Some(axis("pitch angle", "deg")));

    assert!(exported(&doc, &catalogue, &events, Some((10.0, 30.0))).is_ok());
    assert_eq!(
        strictness_warning(&catalogue).as_deref(),
        Some(
            "hand made: TFCat knows shapes in frequency, wavelength, energy or wavenumber, \
             not in pitch angle, so this file is not strict TFCat. \
             VOTable and tscat JSON keep the shapes exactly."
        )
    );
    for known in ["frequency", "wavelength", "energy", "wavenumber"] {
        let (_, catalogue, _) = described(Vec::new(), Some(axis(known, "x")));
        assert_eq!(strictness_warning(&catalogue), None, "{known}");
    }
    assert_eq!(strictness_warning(&plain(Vec::new()).1), None);
}
