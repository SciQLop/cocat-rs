//! The HELIO4CAST ICME catalogue (Möstl et al.): interplanetary coronal mass ejections seen
//! in situ, one public catalogue per observing spacecraft.
//!
//! The data is one CSV file, `https://helioforecast.space/static/sync/icmecat/
//! HELIO4CAST_ICMECAT_v<N>.csv`, whose version changes with each release. The page
//! `https://helioforecast.space/icmecat` links the current one, so a run reads the page first.
//! The page also offers JSON, XLSX, HDF5 and pickles of the same table.
//!
//! An event runs from the ICME's start to the end of its magnetic obstacle. Its other
//! parameters keep the catalogue's names, with their unit added: `mo_bmax_nt`.

use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;
use cocat_core::model::EventRecord;
use cocat_core::time::Timestamp;
use scraper::{Html, Selector};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::{Fetched, Provider};

pub struct Icmecat;

const NAMESPACE: Uuid = Uuid::from_u128(0x9c4e7a15_2d3b_4f86_a1e0_5b7d3c8e6f42);
const PAGE: &str = "https://helioforecast.space/icmecat";
const DESCRIPTION: &str = "Interplanetary coronal mass ejections seen in situ by {}, from the HELIO4CAST ICMECAT (Moestl et al.), https://helioforecast.space/icmecat. If results from this catalogue go into a peer-reviewed paper, the authors ask to be contacted about co-authorship.";
/// The columns that are not attributes: the event's key, its catalogue and its two ends.
const STRUCTURE: [&str; 5] = [
    "",
    "icmecat_id",
    "sc_insitu",
    "icme_start_time",
    "mo_end_time",
];
/// How the file names the spacecraft whose name it shortens.
const OBSERVERS: [(&str, &str); 3] = [
    ("SolarOrbiter", "Solar Orbiter"),
    ("ULYSSES", "Ulysses"),
    ("PSP", "Parker Solar Probe"),
];
/// What a column's name holds, and the unit it is in, from the catalogue's header file.
const UNITS: [(&str, &str); 9] = [
    ("heliodistance", "au"),
    ("_long_", "deg"),
    ("_lat_", "deg"),
    ("duration", "h"),
    ("_b", "nt"),
    ("speed", "km_s"),
    ("pdyn", "npa"),
    ("density", "cm3"),
    ("temperature", "k"),
];

/// The CSV the page links, whatever its version.
fn csv_url(page: &str) -> Result<String, String> {
    let links = Selector::parse("a[href]").map_err(|error| error.to_string())?;
    let base = reqwest::Url::parse(PAGE).map_err(|error| error.to_string())?;
    Html::parse_document(page)
        .select(&links)
        .filter_map(|link| link.attr("href"))
        .filter(|href| href.contains("ICMECAT") && href.ends_with(".csv"))
        .find_map(|href| base.join(href).ok())
        .map(String::from)
        .ok_or_else(|| "the page links no CSV, has its layout changed?".to_owned())
}

/// A table row by column name.
type Row = HashMap<String, String>;

fn lists(body: &str) -> Result<Vec<Fetched>, String> {
    let mut reader = csv::Reader::from_reader(body.as_bytes());
    let headers = reader.headers().map_err(|error| error.to_string())?.clone();
    let columns: Vec<String> = headers
        .iter()
        .filter(|name| !STRUCTURE.contains(name))
        .map(attribute_name)
        .collect();
    let mut by_observer = BTreeMap::<String, Vec<EventRecord>>::new();
    for record in reader.records() {
        let row: Row = record
            .and_then(|record| record.deserialize(Some(&headers)))
            .map_err(|error| error.to_string())?;
        let (observer, event) = event(&row)?;
        by_observer.entry(observer).or_default().push(event);
    }
    if by_observer.is_empty() {
        return Err("no event found, has the file layout changed?".to_owned());
    }
    Ok(by_observer
        .into_iter()
        .map(|(observer, events)| catalogue(&observer, &columns, events))
        .collect())
}

fn event(row: &Row) -> Result<(String, EventRecord), String> {
    let id = field(row, "icmecat_id")?;
    let time =
        |column: &str| icme_time(field(row, column)?).ok_or_else(|| format!("{id}: bad {column}"));
    let uuid = Uuid::new_v5(&NAMESPACE, id.as_bytes()).to_string();
    let mut record = EventRecord::new(uuid, time("icme_start_time")?, time("mo_end_time")?);
    record.attributes = attributes(row).map_err(|error| format!("{id}: {error}"))?;
    Ok((field(row, "sc_insitu")?.to_owned(), record))
}

fn field<'a>(row: &'a Row, column: &str) -> Result<&'a str, String> {
    row.get(column)
        .map(String::as_str)
        .ok_or_else(|| format!("a row has no {column}"))
}

/// The file writes `2025-08-10T05:17Z`, without seconds.
fn icme_time(text: &str) -> Option<Timestamp> {
    Timestamp::parse(&text.replace('Z', ":00Z"))
}

fn attributes(row: &Row) -> Result<Map<String, Value>, String> {
    let values = row
        .iter()
        .filter(|(column, value)| !STRUCTURE.contains(&column.as_str()) && !value.is_empty());
    values
        .map(|(column, value)| Ok((attribute_name(column), attribute_value(column, value)?)))
        .collect()
}

fn attribute_value(column: &str, text: &str) -> Result<Value, String> {
    let bad = || format!("{column} is {text}");
    if column.ends_with("_time") {
        let at = icme_time(text).ok_or_else(bad)?;
        return Ok(Value::String(at.rfc3339().map_err(|_| bad())?));
    }
    let number: f64 = text.parse().map_err(|_| bad())?;
    serde_json::Number::from_f64(number)
        .map(Value::Number)
        .ok_or_else(bad)
}

fn attribute_name(column: &str) -> String {
    match UNITS.iter().find(|(part, _)| column.contains(part)) {
        Some((_, unit)) => format!("{column}_{unit}"),
        None => column.to_owned(),
    }
}

fn observer_name(observer: &str) -> &str {
    OBSERVERS
        .iter()
        .find(|(raw, _)| *raw == observer)
        .map_or(observer, |(_, name)| name)
}

fn catalogue(observer: &str, columns: &[String], events: Vec<EventRecord>) -> Fetched {
    let key = observer.to_lowercase();
    let name = observer_name(observer);
    Fetched {
        catalogue: Uuid::new_v5(&NAMESPACE, key.as_bytes()).to_string(),
        key,
        folder: "ICMECAT".to_owned(),
        name: name.to_owned(),
        description: DESCRIPTION.replace("{}", name),
        url: PAGE.to_owned(),
        columns: columns.to_vec(),
        events: Ok(events),
    }
}

async fn download(http: &reqwest::Client, url: &str) -> Result<String, String> {
    let response = http.get(url).send().await.map_err(super::fetch_error)?;
    response
        .error_for_status()
        .map_err(super::fetch_error)?
        .text()
        .await
        .map_err(super::fetch_error)
}

#[async_trait]
impl Provider for Icmecat {
    fn id(&self) -> &'static str {
        "icmecat"
    }

    fn source(&self) -> &'static str {
        "HELIO4CAST ICMECAT"
    }

    fn default_schedule(&self) -> &'static str {
        "30 6 * * 1"
    }

    async fn fetch(
        &self,
        http: &reqwest::Client,
        _settings: &Value,
        _refreshed: &HashMap<String, Timestamp>,
    ) -> Result<Vec<Fetched>, String> {
        let url = csv_url(&download(http, PAGE).await?)?;
        lists(&download(http, &url).await?)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const SAMPLE: &str = include_str!("../../tests/fixtures/relay/icmecat_sample.csv");
    const PAGE_LINKS: &str = include_str!("../../tests/fixtures/relay/icmecat_page_links.html");

    fn at(text: &str) -> Timestamp {
        Timestamp::parse(text).unwrap()
    }

    fn list(key: &str) -> Fetched {
        lists(SAMPLE)
            .unwrap()
            .into_iter()
            .find(|list| list.key == key)
            .unwrap()
    }

    fn event(key: &str, id: &str) -> EventRecord {
        let uuid = Uuid::new_v5(&NAMESPACE, id.as_bytes()).to_string();
        list(key)
            .events
            .unwrap()
            .into_iter()
            .find(|event| event.uuid == uuid)
            .unwrap()
    }

    #[test]
    fn the_csv_is_the_one_the_page_links() {
        assert_eq!(
            csv_url(PAGE_LINKS).unwrap(),
            "https://helioforecast.space/static/sync/icmecat/HELIO4CAST_ICMECAT_v23.csv"
        );
    }

    #[test]
    fn a_page_without_the_csv_is_an_error() {
        assert!(csv_url("<html><a href=\"x.json\">json</a></html>").is_err());
    }

    #[test]
    fn each_spacecraft_has_its_catalogue_under_icmecat() {
        let lists = lists(SAMPLE).unwrap();

        let mut names: Vec<_> = lists.iter().map(|list| list.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "BepiColombo",
                "Juno",
                "MAVEN",
                "MESSENGER",
                "Parker Solar Probe",
                "STEREO-A",
                "STEREO-B",
                "Solar Orbiter",
                "Ulysses",
                "VEX",
                "Wind"
            ]
        );
        let solo = list("solarorbiter");
        assert_eq!(solo.folder, "ICMECAT");
        assert_eq!(solo.name, "Solar Orbiter");
        assert_eq!(solo.url, PAGE);
        assert!(solo.description.contains("co-authorship"));
        assert_eq!(solo.events.unwrap().len(), 2);
    }

    #[test]
    fn an_event_runs_from_the_icme_start_to_the_end_of_its_obstacle() {
        let event = event("solarorbiter", "ICME_SOLO_MOESTL_20250527_01");

        assert_eq!(event.start, at("2025-05-27T01:20:00Z"));
        assert_eq!(event.stop, at("2025-05-27T11:27:00Z"));
    }

    #[test]
    fn parameters_keep_their_names_with_the_unit() {
        let event = event("stereo-a", "ICME_STEREO_A_MOESTL_20250810_01");
        let a = &event.attributes;

        assert_eq!(a["mo_sc_heliodistance_au"], json!(0.9587));
        assert_eq!(a["mo_sc_long_heeq_deg"], json!(44.72));
        assert_eq!(a["icme_duration_h"], json!(27.05));
        assert_eq!(a["icme_bmax_nt"], json!(21.3));
        assert_eq!(a["mo_bzmin_nt"], json!(-8.9));
        assert_eq!(a["icme_speed_mean_km_s"], json!(504.1));
        assert_eq!(a["mo_pdyn_mean_npa"], json!(0.3));
        assert_eq!(a["mo_density_mean_cm3"], json!(0.7));
        assert_eq!(a["mo_temperature_mean_k"], json!(0.0));
        assert_eq!(a["mo_start_time"], json!("2025-08-10T13:07:00Z"));
        assert!(!a.contains_key("icmecat_id") && !a.contains_key("sc_insitu"));
        assert!(!a.contains_key("icme_start_time") && !a.contains_key("mo_end_time"));
    }

    #[test]
    fn a_value_the_catalogue_leaves_empty_is_no_attribute() {
        let event = event("stereo-a", "ICME_STEREO_A_MOESTL_20250810_01");

        assert!(!event.attributes.contains_key("mo_expansion_speed_km_s"));
        let bepi = self::event("bepicolombo", "ICME_BEPI_MOESTL_20241216_01");
        assert!(!bepi.attributes.contains_key("mo_pdyn_mean_npa"));
        assert!(bepi.attributes.contains_key("mo_sc_lat_heeq_deg"));
    }

    #[test]
    fn the_columns_follow_the_files_order() {
        let columns = list("wind").columns;

        assert_eq!(columns[0], "mo_start_time");
        assert_eq!(columns[1], "mo_sc_heliodistance_au");
        assert_eq!(columns.last().unwrap(), "sheath_pdyn_std_npa");
        assert_eq!(columns.len(), 34);
    }

    #[test]
    fn an_obstacle_that_starts_after_the_icme_still_ends_after_it_starts() {
        let event = event("stereo-a", "ICME_STEREO_A_JIAN_20120315_01");

        assert!(event.start < event.stop);
        assert_eq!(
            event.attributes["mo_start_time"],
            json!("2012-03-15T04:10:00Z")
        );
    }

    #[test]
    fn an_empty_file_is_an_error() {
        let header = SAMPLE.lines().next().unwrap();

        assert!(lists(header).is_err());
        assert!(lists("").is_err());
    }

    #[test]
    fn a_bad_time_is_an_error() {
        let broken = SAMPLE.replace("2025-05-27T01:20Z", "yesterday");

        assert!(
            lists(&broken)
                .unwrap_err()
                .contains("ICME_SOLO_MOESTL_20250527_01")
        );
    }

    #[test]
    fn the_provider_is_registered_with_no_setting() {
        let provider = crate::relay::providers()
            .into_iter()
            .find(|provider| provider.id() == "icmecat")
            .unwrap();

        assert_eq!(provider.source(), "HELIO4CAST ICMECAT");
        assert!(provider.settings().is_empty());
    }
}
