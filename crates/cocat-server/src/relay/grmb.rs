//! The Cluster GRMB region catalogue (Grison et al. 2025, Scientific Data 12, A327,
//! doi:10.1038/s41597-025-04639-z) from the ESA Cluster Science Archive: which region of
//! geospace each spacecraft was in, one catalogue per spacecraft.
//!
//! The archive answers with a gzip-compressed tar holding one CEF file (Cluster Exchange
//! Format, ASCII). Its header names the columns, its records follow `DATA_UNTIL = END_DATA`.

use std::collections::HashMap;
use std::io::Read;
use std::time::Duration;

use async_trait::async_trait;
use cocat_core::model::EventRecord;
use cocat_core::time::Timestamp;
use flate2::read::GzDecoder;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::{Fetched, Provider};

pub struct Grmb;
const NAMESPACE: Uuid = Uuid::from_u128(0x3b8d5f27_c140_4e9a_8a62_d17e0f5b9c34);
const ENDPOINT: &str = "https://csa.esac.esa.int/csa-sl-tap/data";
const SPACECRAFT: [u8; 4] = [1, 2, 3, 4];
const BUSY_PAUSE: Duration = Duration::from_secs(30);
const PAGE: &str = "https://csa.esac.esa.int";
const DESCRIPTION: &str = "The region of geospace Cluster {} was in, from the Geospace Region and Magnetospheric Boundary identification (GRMB): 15 labels chosen by hand from 44 products of the Cluster Science Archive, at a resolution of about 20 minutes. Source: ESA Cluster Science Archive https://csa.esac.esa.int, dataset doi:10.6084/m9.figshare.27952410. Please cite Grison et al. 2025, Scientific Data 12, A327, doi:10.1038/s41597-025-04639-z.";
/// The columns of the catalogue, its attributes in order.
const COLUMNS: [&str; 7] = [
    "region",
    "region_name",
    "region_code",
    "inbound_outbound",
    "complex_crossing",
    "quality_code",
    "quality_products",
];
/// The CEF variables that are attributes, with the attribute each one is.
const VARIABLES: [(&str, &str); 6] = [
    ("location_label", "region"),
    ("location_code", "region_code"),
    ("inbound_vs_outbound", "inbound_outbound"),
    ("crossing_complexity", "complex_crossing"),
    ("quality_location_code", "quality_code"),
    ("quality_location_label", "quality_products"),
];
/// The labels, with the names the CEF header gives them. `NO_DATA` is the label of a record
/// with no data, which is no event.
const NO_DATA: &str = "N/A";
const REGIONS: [(&str, &str); 15] = [
    ("IN/UKN", "Inside the magnetosphere"),
    ("IN/PLS", "Plasmasphere"),
    ("IN/PPTR", "Plasmapause Transition Region"),
    ("IN/PSH", "Plasmasheet"),
    ("IN/PSTR", "Plasmasheet Transition Region"),
    ("IN/LOB", "Lobe"),
    ("IN/POL", "Polar Regions"),
    ("IN/MP", "Magnetopause"),
    ("IN/MPTR", "Magnetopause Transition Region"),
    ("OUT/MSH", "Magnetosheath"),
    ("OUT/BSTR", "Bow Shock Transition Region"),
    ("OUT/SWF", "Solar Wind and Foreshock"),
    ("OUT/UKN", "Outside the magnetosphere"),
    ("UNKNOWN", "Available data are not conclusive"),
    (NO_DATA, "No data"),
];

fn request_url(spacecraft: u8, until: Timestamp) -> Result<String, String> {
    let until = until.seconds_text().map_err(|error| error.to_string())?;
    let until = format!("{}Z", until.replace(' ', "T"));
    let params = [
        ("RETRIEVAL_TYPE", "product"),
        ("DATASET_ID", &format!("C{spacecraft}_CT_AUX_GRMB")),
        ("START_DATE", "2001-01-01T00:00:00Z"),
        ("END_DATE", &until),
        ("DELIVERY_INTERVAL", "All"),
        ("FLAT", "True"),
    ];
    reqwest::Url::parse_with_params(ENDPOINT, params)
        .map(String::from)
        .map_err(|error| error.to_string())
}

/// The CEF file inside the archive the CSA answers with.
// simplify: read_to_string has no size cap; the source is ESA and the archive under 1 MB.
// Upgrade: cap the entry with `take`.
fn cef_text(archive: &[u8]) -> Result<String, String> {
    let mut archive = tar::Archive::new(GzDecoder::new(archive));
    for entry in archive.entries().map_err(|error| error.to_string())? {
        let mut entry = entry.map_err(|error| error.to_string())?;
        let is_cef = entry
            .path()
            .is_ok_and(|path| path.extension().is_some_and(|e| e == "cef"));
        if is_cef {
            let mut text = String::new();
            entry
                .read_to_string(&mut text)
                .map_err(|error| error.to_string())?;
            return Ok(text);
        }
    }
    Err("the archive holds no CEF file".to_owned())
}

/// The variables in the order the header declares them, without the `__C1_CT_AUX_GRMB` suffix.
fn variables(cef: &str) -> Vec<String> {
    cef.lines()
        .filter_map(|line| line.strip_prefix("START_VARIABLE"))
        .filter_map(|rest| rest.split('=').nth(1))
        .map(|name| {
            name.trim()
                .split("__")
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

/// The record lines: between `DATA_UNTIL = END_DATA` and `END_DATA`, without comments.
fn data_lines(cef: &str) -> impl Iterator<Item = &str> {
    cef.lines()
        .skip_while(|line| !line.starts_with("DATA_UNTIL"))
        .skip(1)
        .take_while(|line| line.trim() != "END_DATA")
        .filter(|line| !line.trim().is_empty() && !line.starts_with('!'))
}

fn event_uuid(key: &str, start: &str) -> String {
    Uuid::new_v5(&NAMESPACE, format!("{key} {start}").as_bytes()).to_string()
}

fn records(key: &str, cef: &str) -> Result<Vec<EventRecord>, String> {
    let names = variables(cef);
    let events = data_lines(cef)
        .map(|line| record(key, &names, line))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if events.is_empty() {
        return Err("no event found, has the file layout changed?".to_owned());
    }
    Ok(events)
}

/// `None` for a record with no data, or that lasts no time: the operator's slips, which
/// the next record, starting at the same time, replaces (they would share its key).
/// No label holds a comma, so splitting on commas is enough.
fn record(key: &str, names: &[String], line: &str) -> Result<Option<EventRecord>, String> {
    let fields = line.split(',').map(|field| field.trim().trim_matches('"'));
    let row: HashMap<&str, &str> = names.iter().map(String::as_str).zip(fields).collect();
    let bad = |what: &str| format!("{what} in {line}");
    let column = |name: &str| {
        row.get(name)
            .copied()
            .ok_or_else(|| bad(&format!("no {name}")))
    };
    let region = column("location_label")?;
    let (start, stop) = column("time_tags")?
        .split_once('/')
        .ok_or_else(|| bad("bad time range"))?;
    if region == NO_DATA || start == stop {
        return Ok(None);
    }
    let time = |text: &str| Timestamp::parse(text).ok_or_else(|| bad("bad time"));
    let mut event = EventRecord::new(event_uuid(key, start), time(start)?, time(stop)?);
    event.attributes = attributes(region, &column_values(&column)?)?;
    Ok(Some(event))
}

fn column_values<'a>(
    column: &impl Fn(&str) -> Result<&'a str, String>,
) -> Result<Vec<(&'static str, &'a str)>, String> {
    VARIABLES
        .iter()
        .map(|(variable, attribute)| Ok((*attribute, column(variable)?)))
        .collect()
}

fn attributes(region: &str, values: &[(&str, &str)]) -> Result<Map<String, Value>, String> {
    let name = REGIONS
        .iter()
        .find(|(label, _)| *label == region)
        .map_or(region, |(_, name)| name);
    let mut map = Map::new();
    for (attribute, text) in values {
        map.insert((*attribute).to_owned(), attribute_value(attribute, text)?);
    }
    map.insert("region_name".to_owned(), json!(name));
    Ok(map)
}

fn attribute_value(attribute: &str, text: &str) -> Result<Value, String> {
    match attribute {
        "region" | "quality_products" => Ok(json!(text)),
        _ => text
            .parse::<i64>()
            .map(|number| json!(number))
            .map_err(|_| format!("{attribute} is {text}")),
    }
}

fn catalogue(spacecraft: u8, events: Result<Vec<EventRecord>, String>) -> Fetched {
    let key = format!("c{spacecraft}");
    Fetched {
        catalogue: Uuid::new_v5(&NAMESPACE, key.as_bytes()).to_string(),
        name: format!("C{spacecraft} regions"),
        description: DESCRIPTION.replace("{}", &format!("C{spacecraft}")),
        key,
        folder: "Cluster/GRMB".to_owned(),
        url: PAGE.to_owned(),
        columns: COLUMNS.map(str::to_owned).to_vec(),
        events,
    }
}

async fn fetch_spacecraft(http: &reqwest::Client, spacecraft: u8, until: Timestamp) -> Fetched {
    let events = async {
        let url = request_url(spacecraft, until)?;
        let archive = super::patient::fetch(http, &url, BUSY_PAUSE).await?;
        records(&format!("c{spacecraft}"), &cef_text(&archive)?)
    }
    .await;
    catalogue(spacecraft, events)
}

#[async_trait]
impl Provider for Grmb {
    fn id(&self) -> &'static str {
        "grmb"
    }

    fn source(&self) -> &'static str {
        "Cluster GRMB (ESA CSA, BIRA-IASB)"
    }

    fn default_schedule(&self) -> &'static str {
        "0 4 * * 1"
    }

    async fn fetch(
        &self,
        http: &reqwest::Client,
        _settings: &Value,
        _refreshed: &HashMap<String, Timestamp>,
    ) -> Result<Vec<Fetched>, String> {
        let until = Timestamp::now();
        // simplify: one spacecraft at a time on purpose, to be polite to the CSA.
        let mut lists = Vec::new();
        for spacecraft in SPACECRAFT {
            lists.push(fetch_spacecraft(http, spacecraft, until).await);
        }
        Ok(lists)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::GzEncoder;
    use serde_json::json;

    use super::*;

    const SAMPLE: &str = include_str!("../../tests/fixtures/relay/grmb_sample.cef");
    const SWAPPED: &str = include_str!("../../tests/fixtures/relay/grmb_swapped.cef");

    fn at(text: &str) -> Timestamp {
        Timestamp::parse(text).unwrap()
    }

    fn events() -> Vec<EventRecord> {
        records("c1", SAMPLE).unwrap()
    }

    fn event_at(start: &str) -> EventRecord {
        let uuid = event_uuid("c1", start);
        events().into_iter().find(|e| e.uuid == uuid).unwrap()
    }

    fn archive(name: &str, body: &str) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_cksum();
        tar.append_data(&mut header, name, body.as_bytes()).unwrap();
        let mut gzip = GzEncoder::new(Vec::new(), Compression::fast());
        gzip.write_all(&tar.into_inner().unwrap()).unwrap();
        gzip.finish().unwrap()
    }

    #[test]
    fn the_variable_order_is_read_from_the_header() {
        let names = variables(SAMPLE);

        assert_eq!(names[0], "time_tags");
        assert_eq!(names[1], "location_label");
        assert_eq!(names.len(), 7);
    }

    #[test]
    fn a_record_becomes_an_event_with_its_attributes() {
        let event = event_at("2001-01-01T00:02:56Z");

        assert_eq!(event.start, at("2001-01-01T00:02:56Z"));
        assert_eq!(event.stop, at("2001-01-01T00:19:16Z"));
        let a = &event.attributes;
        assert_eq!(a["region"], json!("OUT/BSTR"));
        assert_eq!(a["region_name"], json!("Bow Shock Transition Region"));
        assert_eq!(a["region_code"], json!(11));
        assert_eq!(a["inbound_outbound"], json!(3));
        assert_eq!(a["complex_crossing"], json!(0));
        assert_eq!(a["quality_code"], json!(4));
        assert_eq!(
            a["quality_products"],
            json!("41515352187100000276753153200000")
        );
    }

    #[test]
    fn records_without_data_and_comments_are_skipped() {
        let regions: Vec<_> = events()
            .iter()
            .map(|e| e.attributes["region"].clone())
            .collect();

        assert_eq!(events().len(), 6);
        assert!(!regions.contains(&json!("N/A")));
        assert!(regions.contains(&json!("UNKNOWN")));
    }

    #[test]
    fn a_record_lasting_no_time_is_skipped_for_the_one_starting_with_it() {
        let events = events();

        assert_eq!(
            events
                .iter()
                .filter(|e| e.start == at("2002-10-14T08:01:01Z"))
                .count(),
            1
        );
        assert_eq!(
            event_at("2002-10-14T08:01:01Z").attributes["region"],
            json!("IN/PSTR")
        );
    }

    #[test]
    fn an_event_keeps_its_uuid_and_differs_by_spacecraft() {
        let first = event_uuid("c1", "2001-01-01T00:02:56Z");

        assert_eq!(first, event_at("2001-01-01T00:02:56Z").uuid);
        assert_ne!(first, event_uuid("c2", "2001-01-01T00:02:56Z"));
    }

    #[test]
    fn every_label_of_the_file_has_a_name() {
        for event in events() {
            let region = event.attributes["region"].as_str().unwrap().to_owned();
            assert!(
                REGIONS.iter().any(|(label, _)| *label == region),
                "{region}"
            );
        }
        assert_eq!(REGIONS.len(), 15);
    }

    #[test]
    fn swapped_columns_still_parse_right() {
        let events = records("c1", SWAPPED).unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].attributes["region"], json!("OUT/BSTR"));
        assert_eq!(events[0].attributes["region_code"], json!(11));
        assert_eq!(events[0].attributes["inbound_outbound"], json!(3));
    }

    #[test]
    fn a_file_without_records_or_a_column_is_an_error() {
        let no_label = SAMPLE.replace("location_label__", "label__");

        assert!(records("c1", "DATA_UNTIL = END_DATA\nEND_DATA\n").is_err());
        assert!(records("c1", &no_label).is_err());
        assert!(records("c1", "").is_err());
    }

    #[test]
    fn a_bad_record_is_an_error() {
        let broken = SAMPLE.replace("2001-01-01T00:02:56Z/", "yesterday/");

        assert!(records("c1", &broken).is_err());
    }

    #[test]
    fn the_cef_is_read_out_of_the_archive() {
        let bytes = archive("CSA_Download_1/C1_CT_AUX_GRMB__x.cef", SAMPLE);

        assert_eq!(cef_text(&bytes).unwrap(), SAMPLE);
        assert!(cef_text(&archive("readme.txt", "x")).is_err());
        assert!(cef_text(b"not an archive").is_err());
    }

    #[test]
    fn each_spacecraft_has_its_url() {
        let url = request_url(3, at("2026-11-11T12:30:45Z")).unwrap();

        assert!(url.starts_with("https://csa.esac.esa.int/csa-sl-tap/data?"));
        assert!(url.contains("DATASET_ID=C3_CT_AUX_GRMB"));
        assert!(url.contains("START_DATE=2001-01-01T00%3A00%3A00Z"));
        assert!(url.contains("END_DATE=2026-11-11T12%3A30%3A45Z"));
        assert!(url.contains("DELIVERY_INTERVAL=All") && url.contains("FLAT=True"));
        assert!(
            request_url(1, at("2026-01-01T00:00:00Z"))
                .unwrap()
                .contains("C1_CT")
        );
        assert!(
            request_url(4, at("2026-01-01T00:00:00Z"))
                .unwrap()
                .contains("C4_CT")
        );
    }

    #[test]
    fn a_catalogue_is_one_spacecraft_under_cluster_grmb() {
        let list = catalogue(2, Ok(events()));

        assert_eq!(list.key, "c2");
        assert_eq!(list.name, "C2 regions");
        assert_eq!(list.folder, "Cluster/GRMB");
        assert!(list.description.contains("Grison"));
        assert!(list.description.contains("10.6084/m9.figshare.27952410"));
        assert_eq!(list.columns[0], "region");
        assert_eq!(list.columns.len(), 7);
        assert_eq!(list.events.unwrap().len(), 6);
    }

    #[test]
    fn the_provider_is_registered_with_a_weekly_schedule_and_no_setting() {
        let provider = crate::relay::providers()
            .into_iter()
            .find(|provider| provider.id() == "grmb")
            .unwrap();

        assert_eq!(provider.source(), "Cluster GRMB (ESA CSA, BIRA-IASB)");
        assert_eq!(provider.default_schedule(), "0 4 * * 1");
        assert!(provider.settings().is_empty());
    }
}
