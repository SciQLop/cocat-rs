//! CACTus, SIDC's automatic CME detection in SOHO/LASCO images
//! (https://www.sidc.be/cactus/), read from its plain text catalogues under
//! `https://www.sidc.be/cactus/catalog/LASCO/2_5_0/`.
//!
//! `cme_lz.txt` is the level zero list, the reference data set, but it stops in 2015-10 (the
//! level zero images were not reprocessed since). `cme_qkl.txt` is the quick-look list, from
//! 2010-07 to now. Both are published, as two catalogues. The `flow_*` lists (suspicious
//! detections) and the `comb_*` lists (both together) are not.
//!
//! A line holds a CME number, which restarts every month, its onset time and eight figures,
//! split on `|`. The key of a CME is its number and onset time. A line that cannot be read
//! is an error, not a skipped row: a layout change must fail the run.

use std::collections::HashMap;

use async_trait::async_trait;
use cocat_core::model::EventRecord;
use cocat_core::time::Timestamp;
use serde_json::Value;
use time::PrimitiveDateTime;
use time::macros::format_description;
use uuid::Uuid;

use super::{Fetched, Provider};

pub struct Cactus;

struct List {
    key: &'static str,
    name: &'static str,
    description: &'static str,
    data_url: &'static str,
}

const LEVEL_ZERO: List = List {
    key: "lz",
    name: "CACTus LASCO CMEs",
    description: "Coronal mass ejections detected in SOHO/LASCO images, 1997 to 2015, from the level zero CACTus catalogue of SIDC (Royal Observatory of Belgium), https://www.sidc.be/cactus/. The list is fully automatic, without human review.",
    data_url: "https://www.sidc.be/cactus/catalog/LASCO/2_5_0/cme_lz.txt",
};

const QUICK_LOOK: List = List {
    key: "qkl",
    name: "CACTus LASCO CMEs, quick-look",
    description: "Coronal mass ejections detected in SOHO/LASCO quick-look images, 2010 to now, from the quick-look CACTus catalogue of SIDC (Royal Observatory of Belgium), https://www.sidc.be/cactus/. The list is fully automatic, without human review.",
    data_url: "https://www.sidc.be/cactus/catalog/LASCO/2_5_0/cme_qkl.txt",
};

const NAMESPACE: Uuid = Uuid::from_u128(0x7d2f9b46_c803_4a1e_95b7_1e6a0c4d8f53);
const PAGE: &str = "https://www.sidc.be/cactus/";

/// The header's figures after the onset time, in the file's order, and their attributes.
const FIGURES: [&str; 7] = [
    "dt0_h",
    "pa_deg",
    "da_deg",
    "v_km_s",
    "dv_km_s",
    "minv_km_s",
    "maxv_km_s",
];
const HALO: &str = "halo";
const FIELDS: usize = 10;

fn parse(list: &List, body: &str) -> Result<Vec<EventRecord>, String> {
    let cmes = body
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with(['#', ':']))
        .map(|line| cme(list, line).map_err(|error| format!("{error}: {}", line.trim())))
        .collect::<Result<Vec<_>, _>>()?;
    if cmes.is_empty() {
        return Err("no CME found, has the file layout changed?".to_owned());
    }
    Ok(cmes)
}

fn cme(list: &List, line: &str) -> Result<EventRecord, String> {
    let fields: [&str; FIELDS] = line
        .split('|')
        .map(str::trim)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| format!("expected {FIELDS} fields"))?;
    let [number, onset, figures @ .., halo] = fields;
    let start = onset_time(onset)?;
    let figures = figures
        .iter()
        .map(|text| figure(text))
        .collect::<Result<Vec<_>, _>>()?;
    let key = format!("{}/{number}|{onset}", list.key);
    let uuid = Uuid::new_v5(&NAMESPACE, key.as_bytes()).to_string();
    let mut record = EventRecord::new(uuid, start, hours_after(start, figures[0])?);
    record.attributes = FIGURES
        .iter()
        .zip(figures)
        .map(|(name, figure)| ((*name).to_owned(), Value::from(figure)))
        .collect();
    if !halo.is_empty() {
        record.attributes.insert(HALO.to_owned(), halo.into());
    }
    Ok(record)
}

fn figure(text: &str) -> Result<i64, String> {
    text.parse().map_err(|_| format!("{text} is not a number"))
}

fn onset_time(text: &str) -> Result<Timestamp, String> {
    let bad = || format!("{text} is not a time");
    let format = format_description!("[year]/[month]/[day] [hour]:[minute]");
    let at = PrimitiveDateTime::parse(text, format).map_err(|_| bad())?;
    let micros = i128::from(at.assume_utc().unix_timestamp()) * 1_000_000;
    Timestamp::from_unix_micros(micros).ok_or_else(bad)
}

fn hours_after(start: Timestamp, hours: i64) -> Result<Timestamp, String> {
    Timestamp::from_unix_micros(start.unix_micros() + i128::from(hours) * 3_600_000_000)
        .ok_or_else(|| "the liftoff ends out of range".to_owned())
}

fn catalogue(list: &List, events: Vec<EventRecord>) -> Fetched {
    let columns = FIGURES.iter().chain(&[HALO]);
    Fetched {
        key: list.key.to_owned(),
        folder: "SIDC".to_owned(),
        name: list.name.to_owned(),
        description: list.description.to_owned(),
        url: PAGE.to_owned(),
        catalogue: Uuid::new_v5(&NAMESPACE, list.key.as_bytes()).to_string(),
        columns: columns.map(|name| (*name).to_owned()).collect(),
        events: Ok(events),
    }
}

async fn download(http: &reqwest::Client, list: &List) -> Result<String, String> {
    http.get(list.data_url)
        .send()
        .await
        .map_err(super::fetch_error)?
        .error_for_status()
        .map_err(super::fetch_error)?
        .text()
        .await
        .map_err(super::fetch_error)
}

/// A list that could not be downloaded or read comes back unreadable, so that the other one
/// is still kept up to date.
fn read(list: &List, body: Result<String, String>) -> Fetched {
    let events = body
        .and_then(|body| parse(list, &body))
        .map_err(|error| format!("{}: {error}", list.name));
    Fetched {
        events,
        ..catalogue(list, Vec::new())
    }
}

#[async_trait]
impl Provider for Cactus {
    fn id(&self) -> &'static str {
        "cactus"
    }

    fn source(&self) -> &'static str {
        "CACTus (SIDC)"
    }

    fn default_schedule(&self) -> &'static str {
        "30 7 * * 1"
    }

    async fn fetch(
        &self,
        http: &reqwest::Client,
        _settings: &Value,
        _refreshed: &HashMap<String, Timestamp>,
    ) -> Result<Vec<Fetched>, String> {
        Ok(vec![
            read(&LEVEL_ZERO, download(http, &LEVEL_ZERO).await),
            read(&QUICK_LOOK, download(http, &QUICK_LOOK).await),
        ])
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const LZ_SAMPLE: &str = include_str!("../../tests/fixtures/relay/cactus_cme_lz_sample.txt");
    const QKL_SAMPLE: &str = include_str!("../../tests/fixtures/relay/cactus_cme_qkl_sample.txt");

    fn at(text: &str) -> Timestamp {
        Timestamp::parse(text).unwrap()
    }

    fn cme(key: &str) -> EventRecord {
        let uuid = Uuid::new_v5(&NAMESPACE, format!("lz/{key}").as_bytes()).to_string();
        parse(&LEVEL_ZERO, LZ_SAMPLE)
            .unwrap()
            .into_iter()
            .find(|event| event.uuid == uuid)
            .unwrap()
    }

    #[test]
    fn a_cme_runs_from_its_onset_for_its_liftoff_duration() {
        let cmes = parse(&LEVEL_ZERO, LZ_SAMPLE).unwrap();

        assert_eq!(cmes.len(), 25);
        let cme = cme("0001|1997/05/17 02:41");
        assert_eq!(cme.start, at("1997-05-17T02:41:00Z"));
        assert_eq!(cme.stop, at("1997-05-17T03:41:00Z"));
    }

    #[test]
    fn a_cme_keeps_the_headers_figures_with_their_units() {
        let a = cme("0001|1997/05/17 02:41").attributes;

        assert_eq!(a["dt0_h"], json!(1));
        assert_eq!(a["pa_deg"], json!(101));
        assert_eq!(a["da_deg"], json!(20));
        assert_eq!(a["v_km_s"], json!(1200));
        assert_eq!(a["dv_km_s"], json!(210));
        assert_eq!(a["minv_km_s"], json!(961));
        assert_eq!(a["maxv_km_s"], json!(1490));
        assert!(!a.contains_key("halo"));
    }

    #[test]
    fn the_number_restarts_each_month_so_the_key_holds_the_onset_too() {
        let june = cme("0001|1997/06/07 22:33");
        let july = cme("0001|1997/07/13 01:23");

        assert_ne!(june.uuid, july.uuid);
    }

    #[test]
    fn a_liftoff_of_zero_hours_is_a_point() {
        let cme = cme("0001|1997/06/07 22:33");

        assert_eq!(cme.stop, cme.start);
    }

    #[test]
    fn a_wide_cme_says_how_halo_like_it_is() {
        assert_eq!(cme("0006|1997/09/09 20:06").attributes["halo"], json!("II"));
        assert_eq!(
            cme("0029|1997/10/23 12:42").attributes["halo"],
            json!("III")
        );
        assert_eq!(cme("0036|1997/10/31 02:39").attributes["halo"], json!("IV"));
    }

    #[test]
    fn two_cmes_with_one_onset_stay_two() {
        let first = cme("0014|2005/01/04 11:54");
        let second = cme("0015|2005/01/04 11:54");

        assert_ne!(first.uuid, second.uuid);
    }

    #[test]
    fn the_quick_look_list_reads_the_same_way_in_its_own_catalogue() {
        let cmes = parse(&QUICK_LOOK, QKL_SAMPLE).unwrap();

        assert_eq!(cmes.len(), 15);
        assert_eq!(cmes[0].start, at("2010-07-09T06:54:00Z"));
        let lz = parse(&LEVEL_ZERO, QKL_SAMPLE).unwrap();
        assert_ne!(cmes[0].uuid, lz[0].uuid);
    }

    #[test]
    fn each_list_is_a_catalogue_in_the_sidc_folder() {
        let lists = [(&LEVEL_ZERO, LZ_SAMPLE), (&QUICK_LOOK, QKL_SAMPLE)];

        let fetched: Vec<_> = lists
            .into_iter()
            .map(|(list, body)| catalogue(list, parse(list, body).unwrap()))
            .collect();

        assert_eq!(fetched[0].key, "lz");
        assert_eq!(fetched[0].folder, "SIDC");
        assert_eq!(fetched[0].name, "CACTus LASCO CMEs");
        assert_eq!(fetched[1].key, "qkl");
        assert_ne!(fetched[0].catalogue, fetched[1].catalogue);
        for list in &fetched {
            assert!(list.description.contains("without human review"));
            assert!(list.url.starts_with("https://www.sidc.be/cactus"));
            assert_eq!(
                list.columns,
                [
                    "dt0_h",
                    "pa_deg",
                    "da_deg",
                    "v_km_s",
                    "dv_km_s",
                    "minv_km_s",
                    "maxv_km_s",
                    "halo"
                ]
            );
        }
    }

    #[test]
    fn a_line_that_is_not_a_cme_is_an_error_naming_it() {
        let broken = format!(
            "{LZ_SAMPLE}  0097|1997/13/45 99:99| 01 | 101| 020| 1200| 0210| 0961| 1490|    \n"
        );

        let error = parse(&LEVEL_ZERO, &broken).unwrap_err();

        assert!(error.contains("0097|1997/13/45"), "{error}");
        assert!(parse(&LEVEL_ZERO, &format!("{LZ_SAMPLE}  0097|oops\n")).is_err());
    }

    #[test]
    fn a_list_that_could_not_be_read_comes_back_unreadable_beside_the_other() {
        let down = read(&LEVEL_ZERO, Err("Connection reset".to_owned()));
        let garbled = read(&QUICK_LOOK, Ok("  0001|oops\n".to_owned()));
        let fine = read(&QUICK_LOOK, Ok(QKL_SAMPLE.to_owned()));

        assert_eq!(down.key, "lz");
        assert!(down.events.unwrap_err().contains("Connection reset"));
        assert!(
            garbled
                .events
                .unwrap_err()
                .contains("CACTus LASCO CMEs, quick-look")
        );
        assert_eq!(fine.events.unwrap().len(), 15);
    }

    #[test]
    fn a_file_without_cmes_is_an_error() {
        assert!(parse(&LEVEL_ZERO, ":Issued: now\n# CME | t0\n").is_err());
        assert!(parse(&LEVEL_ZERO, "").is_err());
    }

    #[test]
    fn the_provider_is_registered_with_no_setting() {
        let provider = crate::relay::providers()
            .into_iter()
            .find(|provider| provider.id() == "cactus")
            .unwrap();

        assert_eq!(provider.source(), "CACTus (SIDC)");
        assert!(provider.settings().is_empty());
    }
}
