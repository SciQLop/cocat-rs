//! The STIX flare list: flares seen by Solar Orbiter's X-ray spectrometer, from the STIX data
//! center of FHNW (https://datacenter.stix.i4ds.net).
//!
//! The data center has no documented API. This is the request the `stixdcpy` package makes
//! (https://github.com/i4Ds/stixdcpy, `net.py`, `fetch_flare_list`): a form POST of
//! `start_utc`, `end_utc` and `sort` to `/api/request/flare-list`, answered by a JSON list of
//! flares. A request without dates answers the newest 5000 flares, so that is the most one
//! answers: a window as large is taken as cut off and fails the run. About 70 flares a day
//! is the busiest the list has been, so a request reads 30 days.
//!
//! A first run reads the whole history; later runs read the last 90 days, since the data
//! center revises recent flares (see `rolling`).

use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;
use cocat_core::model::EventRecord;
use cocat_core::time::Timestamp;
use serde_json::{Map, Value};
use time::macros::date;
use time::{Date, Duration};
use uuid::Uuid;

use super::{Fetched, Provider, rolling};

pub struct Stix;

const NAMESPACE: Uuid = Uuid::from_u128(0x4b8e2d70_a1c9_4e53_9f06_d27c81b5a3e8);
const ENDPOINT: &str = "https://datacenter.stix.i4ds.net/api/request/flare-list";
const KEY: &str = "flares";
const FIRST_DAY: Date = date!(2020 - 02 - 01);
const REQUEST_SPAN: Duration = Duration::days(29);
const MAX_ROWS: usize = 5000;
const NOT_AVAILABLE: &str = "NA";

/// The list's fields, in its own order, and the attribute each becomes. The estimated
/// fluxes are base 10 logarithms; `CFL` is the coarse flare locator's helioprojective
/// position as seen from Solar Orbiter, null for every flare seen so far.
const COLUMNS: [(&str, &str); 23] = [
    ("att_in", "att_in"),
    ("peak_UTC", "peak_time"),
    ("LC0_BKG", "lc0_bkg"),
    ("GOES_class", "goes_class"),
    ("GOES_flux", "goes_flux_w_m2"),
    ("goes_estimated_min_class", "goes_estimated_min_class"),
    ("goes_estimated_max_class", "goes_estimated_max_class"),
    ("goes_estimated_mean_class", "goes_estimated_mean_class"),
    (
        "goes_estimated_min_flux",
        "goes_estimated_min_log10_flux_w_m2",
    ),
    (
        "goes_estimated_max_flux",
        "goes_estimated_max_log10_flux_w_m2",
    ),
    (
        "goes_estimated_mean_flux",
        "goes_estimated_mean_log10_flux_w_m2",
    ),
    ("CFL_X_arcsec", "cfl_x_arcsec"),
    ("CFL_Y_arcsec", "cfl_y_arcsec"),
    ("LC0_PEAK_COUNTS_4S", "lc0_peak_counts_4s"),
    ("LC0_BKG_COUNTS_4S", "lc0_bkg_counts_4s"),
    ("LC1_PEAK_COUNTS_4S", "lc1_peak_counts_4s"),
    ("LC1_BKG_COUNTS_4S", "lc1_bkg_counts_4s"),
    ("LC2_PEAK_COUNTS_4S", "lc2_peak_counts_4s"),
    ("LC2_BKG_COUNTS_4S", "lc2_bkg_counts_4s"),
    ("LC3_PEAK_COUNTS_4S", "lc3_peak_counts_4s"),
    ("LC3_BKG_COUNTS_4S", "lc3_bkg_counts_4s"),
    ("LC4_PEAK_COUNTS_4S", "lc4_peak_counts_4s"),
    ("LC4_BKG_COUNTS_4S", "lc4_bkg_counts_4s"),
];

fn parse(body: &[u8]) -> Result<Vec<EventRecord>, String> {
    let flares: Vec<Value> =
        serde_json::from_slice(body).map_err(|error| format!("not a list of flares: {error}"))?;
    // simplify: a 30-day request holds at most 5000 flares, about 166 a day; the busiest
    // stretch so far is about 70. Upgrade: a smaller REQUEST_SPAN, or split a window that
    // reaches the cap and ask both halves.
    if flares.len() >= MAX_ROWS {
        return Err(format!(
            "{} flares in one request, the answer may be cut off at {MAX_ROWS}",
            flares.len()
        ));
    }
    flares.iter().map(flare).collect()
}

fn flare(flare: &Value) -> Result<EventRecord, String> {
    let id = flare
        .get("flare_id")
        .and_then(Value::as_str)
        .ok_or("a flare has no flare_id")?;
    let time = |field: &str| {
        flare
            .get(field)
            .and_then(Value::as_str)
            .and_then(Timestamp::parse)
            .ok_or_else(|| format!("flare {id} has no valid {field}"))
    };
    let uuid = Uuid::new_v5(&NAMESPACE, id.as_bytes()).to_string();
    let mut record = EventRecord::new(uuid, time("start_UTC")?, time("end_UTC")?);
    record.attributes = attributes(flare, time("peak_UTC")?);
    Ok(record)
}

fn attributes(flare: &Value, peak: Timestamp) -> Map<String, Value> {
    let goes_missing = flare["GOES_class"] == NOT_AVAILABLE;
    COLUMNS
        .iter()
        .filter(|(field, _)| !(goes_missing && matches!(*field, "GOES_class" | "GOES_flux")))
        .filter_map(|(field, name)| {
            let value = match *field {
                "peak_UTC" => Value::String(peak.rfc3339().ok()?),
                _ => flare.get(*field).filter(|value| !value.is_null())?.clone(),
            };
            Some(((*name).to_owned(), value))
        })
        .collect()
}

fn catalogue(events: Vec<EventRecord>) -> Fetched {
    Fetched {
        key: KEY.to_owned(),
        folder: "Solar Orbiter".to_owned(),
        name: "STIX flares".to_owned(),
        description: "Solar flares seen by Solar Orbiter's X-ray spectrometer STIX, from the STIX data center, https://datacenter.stix.i4ds.net/. Its GOES class and estimates come from STIX's own counts.".to_owned(),
        url: "https://datacenter.stix.i4ds.net/".to_owned(),
        catalogue: Uuid::new_v5(&NAMESPACE, KEY.as_bytes()).to_string(),
        columns: COLUMNS.iter().map(|(_, name)| (*name).to_owned()).collect(),
        events: Ok(events),
    }
}

fn windows(refreshed: Option<Timestamp>, now: Timestamp) -> Result<Vec<(Date, Date)>, String> {
    rolling::windows(refreshed, now, FIRST_DAY, REQUEST_SPAN)
}

/// The window's last day is read to its end, so the request stops at the next midnight.
fn request_form((first, last): (Date, Date)) -> [(&'static str, String); 3] {
    let midnight = |day: Date| format!("{day}T00:00:00");
    [
        ("start_utc", midnight(first)),
        ("end_utc", midnight(last + Duration::DAY)),
        ("sort", "time".to_owned()),
    ]
}

async fn fetch_window(
    http: &reqwest::Client,
    window: (Date, Date),
) -> Result<Vec<EventRecord>, String> {
    let describe = |error: reqwest::Error| format!("{}: {}", window.0, super::fetch_error(error));
    let body = http
        .post(ENDPOINT)
        .form(&request_form(window))
        .send()
        .await
        .map_err(describe)?
        .error_for_status()
        .map_err(describe)?
        .bytes()
        .await
        .map_err(describe)?;
    parse(&body).map_err(|error| format!("{}: {error}", window.0))
}

#[async_trait]
impl Provider for Stix {
    fn id(&self) -> &'static str {
        "stix"
    }

    fn source(&self) -> &'static str {
        "STIX data center (FHNW)"
    }

    fn default_schedule(&self) -> &'static str {
        "30 4 * * *"
    }

    async fn fetch(
        &self,
        http: &reqwest::Client,
        _settings: &Value,
        refreshed: &HashMap<String, Timestamp>,
    ) -> Result<Vec<Fetched>, String> {
        let mut flares = BTreeMap::new();
        for window in windows(refreshed.get(KEY).copied(), Timestamp::now())? {
            for flare in fetch_window(http, window).await? {
                flares.insert(flare.uuid.clone(), flare);
            }
        }
        Ok(vec![catalogue(flares.into_values().collect())])
    }

    /// Whatever is older than the window was not asked for, so its absence says nothing.
    fn keep(&self, event: &EventRecord) -> bool {
        rolling::window_start(Timestamp::now()).is_ok_and(|window| event.start < window)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const SAMPLE: &[u8] = include_bytes!("../../tests/fixtures/relay/stix_flare_list.json");

    fn at(text: &str) -> Timestamp {
        Timestamp::parse(text).unwrap()
    }

    fn flare(id: &str) -> EventRecord {
        let uuid = Uuid::new_v5(&NAMESPACE, id.as_bytes()).to_string();
        parse(SAMPLE)
            .unwrap()
            .into_iter()
            .find(|event| event.uuid == uuid)
            .unwrap()
    }

    #[test]
    fn a_flare_runs_from_its_start_to_its_end_keyed_by_its_id() {
        let flares = parse(SAMPLE).unwrap();

        assert_eq!(flares.len(), 18);
        let flare = flare("2405112356");
        assert_eq!(flare.start, at("2024-05-11T23:54:10.162Z"));
        assert_eq!(flare.stop, at("2024-05-11T23:59:26.162Z"));
    }

    #[test]
    fn a_flare_keeps_its_peak_counts_and_goes_estimate() {
        let a = flare("2405110115").attributes;

        assert_eq!(a["peak_time"], json!("2024-05-11T01:15:10.013Z"));
        assert_eq!(a["goes_class"], json!("X5.9"));
        assert_eq!(a["goes_flux_w_m2"], json!(0.0005892901099286973));
        assert_eq!(a["goes_estimated_mean_class"], json!("B6"));
        assert_eq!(
            a["goes_estimated_mean_log10_flux_w_m2"],
            json!(-6.204332166938138)
        );
        assert_eq!(a["att_in"], json!(false));
        assert!(a["lc0_peak_counts_4s"].is_number());
        assert!(a["lc4_bkg_counts_4s"].is_number());
        assert!(!a.contains_key("flare_id") && !a.contains_key("_id"));
        assert!(!a.contains_key("start_utc") && !a.contains_key("duration"));
    }

    #[test]
    fn a_goes_class_of_na_is_no_goes_class_and_no_flux() {
        let a = flare("2203312111").attributes;

        assert!(!a.contains_key("goes_class"));
        assert!(!a.contains_key("goes_flux_w_m2"));
        assert_eq!(a["goes_estimated_mean_class"], json!("B3"));
    }

    #[test]
    fn a_missing_value_is_no_attribute() {
        let a = flare("2203312111").attributes;

        assert!(!a.contains_key("goes_estimated_mean_log10_flux_w_m2"));
        assert!(!a.contains_key("cfl_x_arcsec"));
    }

    #[test]
    fn a_located_flare_has_its_location() {
        let mut flares: Vec<Value> = serde_json::from_slice(SAMPLE).unwrap();
        flares[0]["CFL_X_arcsec"] = json!(-512.5);
        flares[0]["CFL_Y_arcsec"] = json!(210);

        let a = &parse(&serde_json::to_vec(&flares).unwrap()).unwrap()[0].attributes;

        assert_eq!(a["cfl_x_arcsec"], json!(-512.5));
        assert_eq!(a["cfl_y_arcsec"], json!(210));
    }

    #[test]
    fn a_goes_class_without_a_number_is_kept_as_is() {
        assert_eq!(flare("2006200235").attributes["goes_class"], json!("A"));
    }

    #[test]
    fn an_attenuated_flare_says_so() {
        assert_eq!(flare("2608250953").attributes["att_in"], json!(true));
    }

    #[test]
    fn no_flares_is_an_empty_list() {
        assert_eq!(parse(b"[]"), Ok(Vec::new()));
    }

    #[test]
    fn a_full_answer_is_a_cut_off_one() {
        let flare: Value = serde_json::from_slice::<Vec<Value>>(SAMPLE)
            .unwrap()
            .remove(0);
        let full: Vec<Value> = (0..MAX_ROWS)
            .map(|n| {
                let mut flare = flare.clone();
                flare["flare_id"] = json!(n.to_string());
                flare
            })
            .collect();

        let error = parse(&serde_json::to_vec(&full).unwrap()).unwrap_err();

        assert!(error.contains("5000"), "{error}");
    }

    #[test]
    fn a_flare_without_a_time_is_an_error_naming_it() {
        let broken = String::from_utf8(SAMPLE.to_vec())
            .unwrap()
            .replace("2024-05-11T23:59:26.162", "later");

        assert!(parse(broken.as_bytes()).unwrap_err().contains("2405112356"));
        assert!(parse(b"{}").is_err());
    }

    #[test]
    fn the_catalogue_sits_in_the_solar_orbiter_folder() {
        let list = catalogue(parse(SAMPLE).unwrap());

        assert_eq!(list.key, "flares");
        assert_eq!(list.folder, "Solar Orbiter");
        assert_eq!(list.name, "STIX flares");
        assert!(list.url.starts_with("https://datacenter.stix.i4ds.net"));
        assert_eq!(list.events.unwrap().len(), 18);
    }

    #[test]
    fn every_attribute_of_the_sample_is_one_of_the_columns() {
        let list = catalogue(parse(SAMPLE).unwrap());

        for event in list.events.unwrap() {
            for name in event.attributes.keys() {
                assert!(list.columns.contains(name), "{name}");
            }
        }
    }

    #[test]
    fn the_first_run_reads_every_30_days_from_launch_and_a_later_one_the_window() {
        let now = at("2026-11-10T12:00:00Z");

        let first = windows(None, now).unwrap();
        let later = windows(Some(at("2026-11-09T04:30:00Z")), now).unwrap();

        assert_eq!(first[0].0.to_string(), "2020-02-01");
        assert_eq!(first[1].0.to_string(), "2020-03-02");
        assert_eq!(first.last().unwrap().1.to_string(), "2026-11-10");
        assert_eq!(later.len(), 4);
        assert_eq!(later[0], (date!(2026 - 08 - 12), date!(2026 - 09 - 10)));
    }

    #[test]
    fn a_window_is_asked_from_its_first_midnight_to_the_one_after_its_last_day() {
        let form = request_form((date!(2026 - 08 - 12), date!(2026 - 08 - 31)));

        assert_eq!(
            form,
            [
                ("start_utc", "2026-08-12T00:00:00".to_owned()),
                ("end_utc", "2026-09-01T00:00:00".to_owned()),
                ("sort", "time".to_owned())
            ]
        );
    }

    #[test]
    fn flares_before_the_window_are_kept() {
        let flares = parse(SAMPLE).unwrap();

        assert!(
            Stix.keep(
                flares
                    .iter()
                    .find(|f| f.start < at("2021-01-01T00:00:00Z"))
                    .unwrap()
            )
        );
        assert!(!Stix.keep(&EventRecord::new("x", Timestamp::now(), Timestamp::now())));
    }

    #[test]
    fn the_provider_is_registered_with_no_setting() {
        let provider = crate::relay::providers()
            .into_iter()
            .find(|provider| provider.id() == "stix")
            .unwrap();

        assert_eq!(provider.source(), "STIX data center (FHNW)");
        assert!(provider.settings().is_empty());
    }
}
