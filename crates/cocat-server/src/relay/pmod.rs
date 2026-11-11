//! The Solar Orbiter catalogue portal of PMOD/WRC (https://projects.pmodwrc.ch/solarorbiter/):
//! the EUI and SPICE event catalogues and the IRIS and Hinode coordinated observations. Each
//! page embeds its rows as JSON in a `<script id="data-json">` element.
//!
//! The pages carry no id: an event's key is made of the fields that define it.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use cocat_core::model::EventRecord;
use cocat_core::time::Timestamp;
use scraper::{Html, Selector};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::catcsv;
use super::{Fetched, Provider};

pub struct Pmod;

const NAMESPACE: Uuid = Uuid::from_u128(0x5e2a9c14_b07d_4d63_8f31_7a6c0e94d2b8);
const PORTAL: &str = "https://projects.pmodwrc.ch/solarorbiter/";
const CREDIT: &str = "From the PMOD/WRC Solar Orbiter catalogue portal. The portal publishes no citation text: see the page for how to acknowledge.";
const DAY_MICROS: i128 = 86_400_000_000;

/// Where a row's start and stop are.
enum Times {
    /// Two columns, a time each.
    Pair(&'static str, &'static str),
    /// One column, `2020-03-19 09:49-16:44`: a day and the hours, the stop on the next day when
    /// it is earlier than the start.
    Range(&'static str),
}

/// What tells one catalogue from the others: its page, how its rows get their times, and the
/// fields that make a row's key, the first group whose fields are all filled.
struct Catalogue {
    key: &'static str,
    name: &'static str,
    page: &'static str,
    about: &'static str,
    times: Times,
    keys: &'static [&'static [&'static str]],
}

const CATALOGUES: [Catalogue; 4] = [
    Catalogue {
        key: "eui_events",
        name: "EUI events",
        page: "events_eui.html",
        about: "Solar events identified in Solar Orbiter EUI Full Sun Imager (FSI) and High Resolution Imager (HRI) observations, including brightenings, eruptions, coronal structures, and transient phenomena.",
        times: Times::Pair("Time start", "Time end"),
        keys: &[&["Time start", "Time end", "Event type (I)", "x"]],
    },
    Catalogue {
        key: "spice_events",
        name: "SPICE interesting events",
        page: "events_spice.html",
        about: "Interesting solar events identified in Solar Orbiter SPICE observations, a SPICE-focused event catalogue complementary to the EUI interesting events catalogue.",
        times: Times::Pair("Time_start", "Time_end"),
        keys: &[
            &["FITS_file_name", "Event_type"],
            &["Time_start", "Study_name", "Event_type"],
        ],
    },
    Catalogue {
        key: "iris_coordination",
        name: "IRIS coordinated observations",
        page: "iris_coordination.html",
        about: "Coordinated observations between Solar Orbiter EUI, SPICE and IRIS, enabling joint studies of the chromosphere, transition region, and corona.",
        times: Times::Range("Time"),
        keys: &[&["Time", "Goal"]],
    },
    Catalogue {
        key: "hinode_coordination",
        name: "Hinode coordinated observations",
        page: "eis_coordination.html",
        about: "Observing campaigns coordinated between Solar Orbiter EUI, SPICE and Hinode for multi-wavelength investigations of the solar activity, chromospheric and coronal dynamics.",
        times: Times::Range("Time (Hinode)"),
        keys: &[&["Time (Hinode)", "Goal"]],
    },
];

type Row = Map<String, Value>;

fn rows(page: &str) -> Result<Vec<Row>, String> {
    let selector = Selector::parse("script#data-json").map_err(|error| error.to_string())?;
    let document = Html::parse_document(page);
    let json = document
        .select(&selector)
        .next()
        .ok_or("the page has no data-json element, has its layout changed?")?
        .text()
        .collect::<String>();
    serde_json::from_str(&json).map_err(|error| format!("data-json: {error}"))
}

/// A field as text, empty when the row has none.
fn text(row: &Row, column: &str) -> String {
    match row.get(column) {
        Some(Value::String(text)) => text.trim().to_owned(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

/// The page writes `NaT` for a time it lacks, and `FALSE` for a link it lacks.
fn placeholder(text: &str) -> bool {
    catcsv::missing(text) || matches!(text.trim(), "NaT" | "FALSE")
}

fn snake_case(column: &str) -> String {
    column
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

fn time_columns(times: &Times) -> Vec<&'static str> {
    match times {
        Times::Pair(start, stop) => vec![start, stop],
        Times::Range(column) => vec![column],
    }
}

fn day_after(at: Timestamp) -> Option<Timestamp> {
    Timestamp::from_unix_micros(at.unix_micros() + DAY_MICROS)
}

/// `2020-03-19 09:49-16:44`, with a ` +1d` note some rows add after it, which the stop
/// already says.
fn range(text: &str) -> Option<(Timestamp, Timestamp)> {
    let mut words = text.split_whitespace();
    let (day, hours) = (words.next()?, words.next()?);
    let (from, to) = hours.split_once('-')?;
    let start = Timestamp::parse(&format!("{day} {from}:00"))?;
    let stop = Timestamp::parse(&format!("{day} {to}:00"))?;
    let stop = if stop < start { day_after(stop)? } else { stop };
    Some((start, stop))
}

/// A row lacking one of its two times is a point at the other.
fn pair(row: &Row, start: &str, stop: &str) -> Option<(Timestamp, Timestamp)> {
    let time = |column| Timestamp::parse(&text(row, column));
    match (time(start), time(stop)) {
        (Some(start), Some(stop)) => Some((start, stop)),
        (Some(at), None) | (None, Some(at)) => Some((at, at)),
        (None, None) => None,
    }
}

fn span(times: &Times, row: &Row) -> Option<(Timestamp, Timestamp)> {
    match times {
        Times::Pair(start, stop) => pair(row, start, stop),
        Times::Range(column) => range(&text(row, column)),
    }
}

fn key(catalogue: &Catalogue, row: &Row) -> Option<String> {
    catalogue.keys.iter().find_map(|columns| {
        let parts: Vec<String> = columns.iter().map(|column| text(row, column)).collect();
        (!parts.iter().any(String::is_empty)).then(|| parts.join("|"))
    })
}

fn attribute_value(value: &Value) -> Option<Value> {
    match value {
        Value::Null => None,
        Value::String(text) if placeholder(text) => None,
        Value::String(text) => catcsv::number_or_text(text.trim()),
        other => Some(other.clone()),
    }
}

fn attributes(catalogue: &Catalogue, row: &Row) -> Map<String, Value> {
    let times = time_columns(&catalogue.times);
    row.iter()
        .filter(|(column, _)| !times.contains(&column.as_str()))
        .filter_map(|(column, value)| Some((snake_case(column), attribute_value(value)?)))
        .collect()
}

fn event(catalogue: &Catalogue, row: &Row) -> Option<EventRecord> {
    let (start, stop) = span(&catalogue.times, row)?;
    let uuid = Uuid::new_v5(&NAMESPACE, key(catalogue, row)?.as_bytes());
    let mut event = EventRecord::new(uuid.to_string(), start, stop);
    event.attributes = attributes(catalogue, row);
    Some(event)
}

// simplify: of rows sharing a key the first stays: the portal repeats some rows to the letter,
// and a repeat that differs has no stabler key to tell it apart.
fn first_of_each(events: Vec<EventRecord>) -> Vec<EventRecord> {
    let mut seen = HashSet::new();
    events
        .into_iter()
        .filter(|event| seen.insert(event.uuid.clone()))
        .collect()
}

fn events(catalogue: &Catalogue, page: &str) -> Result<Vec<EventRecord>, String> {
    let rows = rows(page)?;
    let kept: Vec<EventRecord> = rows
        .iter()
        .filter_map(|row| event(catalogue, row))
        .collect();
    if kept.len() < rows.len() {
        tracing::warn!(
            catalogue = catalogue.key,
            skipped = rows.len() - kept.len(),
            "PMOD rows without a readable time or key"
        );
    }
    match first_of_each(kept) {
        events if events.is_empty() => {
            Err("no event found, has the page layout changed?".to_owned())
        }
        events => Ok(events),
    }
}

fn url(catalogue: &Catalogue) -> String {
    format!("{PORTAL}{}", catalogue.page)
}

fn fetched(catalogue: &Catalogue, page: Result<String, String>) -> Fetched {
    Fetched {
        catalogue: Uuid::new_v5(&NAMESPACE, catalogue.key.as_bytes()).to_string(),
        key: catalogue.key.to_owned(),
        folder: "PMOD/Solar Orbiter".to_owned(),
        name: catalogue.name.to_owned(),
        description: format!("{} {CREDIT} {}", catalogue.about, url(catalogue)),
        url: url(catalogue),
        columns: Vec::new(),
        events: page.and_then(|page| events(catalogue, &page)),
    }
}

#[async_trait]
impl Provider for Pmod {
    fn id(&self) -> &'static str {
        "pmod"
    }

    fn source(&self) -> &'static str {
        "PMOD/WRC Solar Orbiter catalogues"
    }

    fn default_schedule(&self) -> &'static str {
        "15 5 * * 2"
    }

    async fn fetch(
        &self,
        http: &reqwest::Client,
        _settings: &Value,
        _refreshed: &HashMap<String, Timestamp>,
    ) -> Result<Vec<Fetched>, String> {
        let mut lists = Vec::new();
        for catalogue in &CATALOGUES {
            let page = catcsv::download(http, &url(catalogue)).await;
            lists.push(fetched(catalogue, page));
        }
        Ok(lists)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const EUI: &str = include_str!("../../tests/fixtures/relay/pmod_eui.html");
    const SPICE: &str = include_str!("../../tests/fixtures/relay/pmod_spice.html");
    const IRIS: &str = include_str!("../../tests/fixtures/relay/pmod_iris.html");
    const HINODE: &str = include_str!("../../tests/fixtures/relay/pmod_hinode.html");

    fn at(text: &str) -> Timestamp {
        Timestamp::parse(text).unwrap()
    }

    fn catalogue(key: &str) -> &'static Catalogue {
        CATALOGUES.iter().find(|c| c.key == key).unwrap()
    }

    fn parsed(key: &str, page: &str) -> Vec<EventRecord> {
        events(catalogue(key), page).unwrap()
    }

    fn event(key: &str, page: &str, id: &str) -> EventRecord {
        let uuid = Uuid::new_v5(&NAMESPACE, id.as_bytes()).to_string();
        parsed(key, page)
            .into_iter()
            .find(|event| event.uuid == uuid)
            .unwrap()
    }

    fn page_of(rows: Value) -> String {
        format!("<script id=\"data-json\" type=\"application/json\">{rows}</script>")
    }

    #[test]
    fn an_eui_event_has_its_times_and_every_other_field_as_an_attribute() {
        let event = event(
            "eui_events",
            EUI,
            "2021-03-22 10:06:00|2021-03-22 10:22:00|Flare|-472.0",
        );
        let a = &event.attributes;

        assert_eq!(event.start, at("2021-03-22T10:06:00Z"));
        assert_eq!(event.stop, at("2021-03-22T10:22:00Z"));
        assert_eq!(a["event_type_i"], json!("Flare"));
        assert_eq!(a["rho"], json!(0.37));
        assert_eq!(a["solo_sun_distance_au"], json!(0.691730623720589));
        assert_eq!(a["fsi304"], json!(0));
        assert_eq!(a["hri174"], json!(1));
        assert_eq!(a["data_type"], json!("HRI"));
        assert_eq!(a["image"], json!("20210322T1006.png"));
        assert!(!a.contains_key("time_start"));
    }

    #[test]
    fn empty_values_and_placeholders_are_absent() {
        let event = event(
            "eui_events",
            EUI,
            "2021-03-22 10:06:00|2021-03-22 10:22:00|Flare|-472.0",
        );
        let a = &event.attributes;

        for absent in [
            "event_type_ii",
            "event_type_iii",
            "pixel_x",
            "earth_hpc_x_arcsec",
            "iris_link",
            "hinode_link",
        ] {
            assert!(!a.contains_key(absent), "{absent}");
        }
    }

    #[test]
    fn an_eui_event_missing_one_end_is_a_point_at_the_other() {
        let events = parsed("eui_events", EUI);
        let point = |kind: &str| {
            events
                .iter()
                .find(|e| e.attributes["event_type_i"] == kind)
                .unwrap()
        };

        assert_eq!(point("CME").start, at("2024-02-21T20:28:00Z"));
        assert_eq!(point("CME").stop, point("CME").start);
        assert_eq!(point("Prominence").start, at("2025-01-15T00:00:00Z"));
        assert_eq!(point("Prominence").stop, point("Prominence").start);
    }

    #[test]
    fn two_eui_rows_of_one_time_and_type_are_told_apart_by_their_position() {
        let events = parsed("eui_events", EUI);
        let simultaneous = events
            .iter()
            .filter(|e| e.start == at("2023-04-15T02:43:00Z"))
            .count();

        assert_eq!(simultaneous, 2);
        assert_eq!(events.len(), 6);
    }

    #[test]
    fn a_spice_event_is_keyed_by_its_fits_file_and_keeps_its_fields() {
        let event = event(
            "spice_events",
            SPICE,
            "solo_L2_spice-n-ras_20211216T112931_V24_83886420-000-DR6.fits|Active Region",
        );
        let a = &event.attributes;

        assert_eq!(event.start, at("2021-12-16T11:29:31.360Z"));
        assert_eq!(event.stop, at("2021-12-16T14:42:28.679Z"));
        assert_eq!(a["study_name"], json!("SCI_SYNOPTIC_SC_SL04_60.0S_FF"));
        assert_eq!(a["target"], json!("on disk, East"));
        assert_eq!(a["hpc_x_arcsec"], json!(-154.07269));
        assert_eq!(a["dsun_au"], json!(1.0150696));
        assert_eq!(a["carrington_lon_deg"], json!(331.31815141799467));
    }

    #[test]
    fn a_spice_null_is_absent() {
        let events = parsed("spice_events", SPICE);

        let bare = events
            .iter()
            .find(|e| !e.attributes.contains_key("carrington_lon_deg"))
            .unwrap();

        assert!(!bare.attributes.contains_key("earth_visible"));
        assert!(bare.attributes.contains_key("dsun_au"));
    }

    #[test]
    fn one_fits_file_listed_under_two_event_types_is_two_events() {
        let events = parsed("spice_events", SPICE);
        let kinds: Vec<&Value> = events
            .iter()
            .filter(|e| {
                e.attributes["fits_file_name"]
                    .as_str()
                    .unwrap()
                    .contains("20260208T055003")
            })
            .map(|e| &e.attributes["event_type"])
            .collect();

        assert_eq!(kinds, [&json!("Active Region"), &json!("Flare")]);
    }

    #[test]
    fn a_row_repeated_to_the_letter_is_one_event() {
        let events = parsed("spice_events", SPICE);
        let repeated = "solo_L2_spice-n-ras_20211228T123445_V24_100663305-000-DR6.fits";

        let count = events
            .iter()
            .filter(|e| e.attributes["fits_file_name"] == repeated)
            .count();

        assert_eq!(count, 1);
        assert_eq!(events.len(), 5);
    }

    #[test]
    fn a_spice_row_without_a_file_name_is_keyed_by_start_and_study() {
        let page = page_of(json!([{
            "Time_start": "2024-01-01T00:00:00.000", "Time_end": "2024-01-01T01:00:00.000",
            "Event_type": "Jet", "Study_name": "S1", "FITS_file_name": ""
        }]));

        let events = events(catalogue("spice_events"), &page).unwrap();

        assert_eq!(
            events[0].uuid,
            Uuid::new_v5(&NAMESPACE, b"2024-01-01T00:00:00.000|S1|Jet").to_string()
        );
    }

    #[test]
    fn an_iris_time_range_is_a_day_and_two_hours() {
        let event = event(
            "iris_coordination",
            IRIS,
            "2020-03-19 09:49-16:44|Coordination with Solar Orbiter -PHI",
        );
        let a = &event.attributes;

        assert_eq!(event.start, at("2020-03-19T09:49:00Z"));
        assert_eq!(event.stop, at("2020-03-19T16:44:00Z"));
        assert_eq!(
            a["iris_data_description"],
            json!("Very large dense 320-step raster")
        );
        assert_eq!(a["coordinate_x_y_arcsec"], json!("-5\",0\""));
        assert_eq!(a["fov_dx_dy_arcsec"], json!("112\" x 175\""));
        assert!((a["distance_au"].as_f64().unwrap() - 0.94243947594651).abs() < 1e-12);
        assert!((a["solo_earthangle"].as_f64().unwrap() - 5.9091746).abs() < 1e-6);
        assert_eq!(a["spice"], json!(0));
        assert!(!a.contains_key("time"));
    }

    #[test]
    fn an_iris_range_that_ends_earlier_ends_the_next_day() {
        let events = parsed("iris_coordination", IRIS);

        let across = events
            .iter()
            .find(|e| e.start == at("2021-04-24T23:39:00Z"))
            .unwrap();

        assert_eq!(across.stop, at("2021-04-25T02:44:00Z"));
    }

    #[test]
    fn a_range_marked_plus_one_day_still_reads_and_its_twin_row_is_dropped() {
        let events = parsed("iris_coordination", IRIS);
        let twins = events
            .iter()
            .filter(|e| e.start == at("2023-10-13T21:38:00Z"))
            .count();

        assert_eq!(twins, 1);
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn a_hinode_range_reads_like_an_iris_one() {
        let events = parsed("hinode_coordination", HINODE);

        let across = events
            .iter()
            .find(|e| e.start == at("2023-04-10T22:59:00Z"))
            .unwrap();

        assert_eq!(across.stop, at("2023-04-11T00:03:00Z"));
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn spice_links_and_files_of_a_coordination_row_are_text() {
        let events = parsed("hinode_coordination", HINODE);

        let first = events
            .iter()
            .find(|e| e.start == at("2020-04-21T11:30:00Z"))
            .unwrap();

        assert_eq!(first.attributes["study_type"], json!("Single Exposure"));
        assert!(
            first.attributes["spice_files"]
                .as_str()
                .unwrap()
                .starts_with("solo_L2_spice-n-exp_20200421T112428")
        );
    }

    #[test]
    fn an_unexpected_field_and_a_non_numeric_value_still_parse() {
        let page = page_of(json!([{
            "Time start": "2024-01-01 00:00:00", "Time end": "2024-01-01 01:00:00",
            "Event type (I)": "Jet", "x": 10.0,
            "rho": "n/a", "brand new (field)": "yes", "count": 3, "flag": true,
            "nested": {"a": 1}
        }]));

        let events = events(catalogue("eui_events"), &page).unwrap();
        let a = &events[0].attributes;

        assert_eq!(a["rho"], json!("n/a"));
        assert_eq!(a["brand_new_field"], json!("yes"));
        assert_eq!(a["count"], json!(3));
        assert_eq!(a["flag"], json!(true));
        assert_eq!(a["nested"], json!({"a": 1}));
    }

    #[test]
    fn a_numeric_text_becomes_a_number() {
        let page = page_of(json!([{
            "Time": "2024-01-01 00:00-01:00", "Goal": "g", "distance (AU)": "0.5"
        }]));

        let events = events(catalogue("iris_coordination"), &page).unwrap();

        assert_eq!(events[0].attributes["distance_au"], json!(0.5));
    }

    #[test]
    fn a_row_with_no_readable_time_is_skipped_not_fatal() {
        let page = page_of(json!([
            {"Time": "someday", "Goal": "a"},
            {"Time": "2024-01-01 00:00-01:00", "Goal": "b"}
        ]));

        let events = events(catalogue("iris_coordination"), &page).unwrap();

        assert_eq!(events.len(), 1);
    }

    #[test]
    fn a_page_without_its_data_or_without_rows_is_an_error() {
        let none = events(catalogue("eui_events"), "<html></html>").unwrap_err();
        let empty = events(catalogue("eui_events"), &page_of(json!([]))).unwrap_err();

        assert!(none.contains("data-json"));
        assert!(empty.contains("no event"));
    }

    #[test]
    fn each_catalogue_has_its_key_folder_page_and_description() {
        let list = fetched(catalogue("iris_coordination"), Ok(IRIS.to_owned()));

        assert_eq!(list.key, "iris_coordination");
        assert_eq!(list.name, "IRIS coordinated observations");
        assert_eq!(list.folder, "PMOD/Solar Orbiter");
        assert_eq!(
            list.url,
            "https://projects.pmodwrc.ch/solarorbiter/iris_coordination.html"
        );
        assert!(list.description.contains("joint studies"));
        assert!(list.description.contains("PMOD/WRC"));
        assert!(list.description.contains("how to acknowledge"));
        assert_eq!(
            list.catalogue,
            Uuid::new_v5(&NAMESPACE, b"iris_coordination").to_string()
        );
        assert_eq!(list.events.unwrap().len(), 3);
    }

    #[test]
    fn a_page_that_cannot_be_read_fails_its_catalogue_and_the_others_are_relayed() {
        let pages = [
            Ok(EUI.to_owned()),
            Err("HTTP 503".to_owned()),
            Ok(IRIS.to_owned()),
            Ok(HINODE.to_owned()),
        ];

        let lists: Vec<Fetched> = CATALOGUES
            .iter()
            .zip(pages)
            .map(|(c, page)| fetched(c, page))
            .collect();

        assert_eq!(lists.len(), 4);
        assert_eq!(lists[1].key, "spice_events");
        assert!(lists[1].events.as_ref().unwrap_err().contains("HTTP 503"));
        assert!(lists.iter().filter(|l| l.events.is_ok()).count() == 3);
    }

    #[test]
    fn the_provider_is_registered_with_no_setting() {
        let provider = crate::relay::providers()
            .into_iter()
            .find(|provider| provider.id() == "pmod")
            .unwrap();

        assert_eq!(provider.source(), "PMOD/WRC Solar Orbiter catalogues");
        assert!(provider.settings().is_empty());
        assert_eq!(provider.default_schedule(), "15 5 * * 2");
    }
}
