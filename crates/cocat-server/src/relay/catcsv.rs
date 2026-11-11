//! What the HELIO4CAST catalogues (Moestl et al., helioforecast.space) share: one CSV per
//! catalogue with a leading unnamed index column, linked from the catalogue's page under a
//! version-numbered name, read into events whose attributes keep the file's column names with
//! their unit added: `mo_bmax_nt`.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use cocat_core::model::EventRecord;
use cocat_core::time::Timestamp;
use scraper::{Html, Selector};
use serde_json::{Map, Value};
use uuid::Uuid;

const SITE: &str = "helioforecast.space";
const BUSY_PAUSE: Duration = Duration::from_secs(30);
/// How the files name the bodies whose name they shorten or shout.
const OBSERVERS: [(&str, &str); 5] = [
    ("SolarOrbiter", "Solar Orbiter"),
    ("ULYSSES", "Ulysses"),
    ("PSP", "Parker Solar Probe"),
    ("JUNO", "Juno"),
    ("Earth_L1", "Earth L1"),
];
/// Where in a column's name a rule looks.
enum At {
    Prefix,
    Suffix,
    /// A `_`-separated word of the name.
    Word,
    /// A word that is the quantity with a statistic: `bzmin`, `vtmean`.
    Statistic,
}

const STATISTICS: [&str; 4] = ["max", "min", "mean", "std"];

/// What a column's name holds, and the unit it is in, from the catalogues' header files. The
/// first match wins. Anchored so that a future column that merely contains `b` gets no unit.
const UNITS: [(At, &str, &str); 19] = [
    (At::Suffix, "heliodistance", "au"),
    (At::Suffix, "_distance", "au"),
    (At::Suffix, "_long_heeq", "deg"),
    (At::Suffix, "_lat_heeq", "deg"),
    (At::Suffix, "_heeq_lon", "deg"),
    (At::Suffix, "_heeq_lat", "deg"),
    (At::Suffix, "_delta", "deg"),
    (At::Prefix, "pa_", "deg"),
    (At::Suffix, "_duration", "h"),
    (At::Suffix, "_time_err", "h"),
    (At::Statistic, "b", "nt"),
    (At::Statistic, "bt", "nt"),
    (At::Statistic, "by", "nt"),
    (At::Statistic, "bz", "nt"),
    (At::Word, "speed", "km_s"),
    (At::Statistic, "vt", "km_s"),
    (At::Word, "pdyn", "npa"),
    (At::Word, "density", "cm3"),
    (At::Word, "temperature", "k"),
];

fn matches(at: &At, part: &str, column: &str) -> bool {
    match at {
        At::Prefix => column.starts_with(part),
        At::Suffix => column.ends_with(part),
        At::Word => column.split('_').any(|word| word == part),
        At::Statistic => column.split('_').any(|word| {
            let statistic = word.strip_prefix(part);
            statistic.is_some_and(|rest| STATISTICS.contains(&rest))
        }),
    }
}

/// A table row by column name.
pub type Row = HashMap<String, String>;

/// One row as an event: the list it belongs to, its key and its ends.
pub struct Entry {
    pub list: String,
    pub key: String,
    pub start: Timestamp,
    pub stop: Timestamp,
}

pub struct Grouped {
    pub columns: Vec<String>,
    pub lists: BTreeMap<String, Vec<EventRecord>>,
}

/// The CSV the page links, whatever its version: the one whose name holds `tag`.
pub fn csv_url(page_url: &str, page: &str, tag: &str) -> Result<String, String> {
    let links = Selector::parse("a[href]").map_err(|error| error.to_string())?;
    let base = reqwest::Url::parse(page_url).map_err(|error| error.to_string())?;
    Html::parse_document(page)
        .select(&links)
        .filter_map(|link| link.attr("href"))
        .filter(|href| href.contains(tag) && href.ends_with(".csv"))
        .find_map(|href| base.join(href).ok())
        .ok_or_else(|| "the page links no CSV, has its layout changed?".to_owned())
        .and_then(on_the_site)
}

fn on_the_site(url: reqwest::Url) -> Result<String, String> {
    match (url.scheme(), url.host_str()) {
        ("https", Some(SITE)) => Ok(url.into()),
        _ => Err(format!(
            "the page links a CSV outside https://{SITE}: {url}"
        )),
    }
}

pub async fn download(http: &reqwest::Client, url: &str) -> Result<String, String> {
    let bytes = super::patient::fetch(http, url, BUSY_PAUSE).await?;
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

/// The table's rows grouped by list, and its columns, the structural ones left out.
pub fn grouped(
    body: &str,
    namespace: &Uuid,
    structure: &[&str],
    entry: impl Fn(&Row) -> Result<Entry, String>,
) -> Result<Grouped, String> {
    let mut reader = csv::Reader::from_reader(body.as_bytes());
    let headers = reader.headers().map_err(|error| error.to_string())?.clone();
    let columns = headers
        .iter()
        .filter(|name| !structure.contains(name))
        .map(attribute_name)
        .collect();
    let mut lists = BTreeMap::<String, Vec<EventRecord>>::new();
    for record in reader.records() {
        let row: Row = record
            .and_then(|record| record.deserialize(Some(&headers)))
            .map_err(|error| error.to_string())?;
        let Entry {
            list,
            key,
            start,
            stop,
        } = entry(&row)?;
        let uuid = Uuid::new_v5(namespace, key.as_bytes()).to_string();
        let mut event = EventRecord::new(uuid, start, stop);
        event.attributes = named(&key, attributes(&row, structure))?;
        lists.entry(list).or_default().push(event);
    }
    match lists.is_empty() {
        true => Err("no event found, has the file layout changed?".to_owned()),
        false => Ok(Grouped { columns, lists }),
    }
}

/// `error` saying which event it is about.
pub fn named<T>(key: &str, result: Result<T, String>) -> Result<T, String> {
    result.map_err(|error| format!("{key}: {error}"))
}

pub fn field<'a>(row: &'a Row, column: &str) -> Result<&'a str, String> {
    row.get(column)
        .map(String::as_str)
        .ok_or_else(|| format!("a row has no {column}"))
}

/// A time column's value, or an error that names the column.
pub fn time(row: &Row, column: &str) -> Result<Timestamp, String> {
    let text = field(row, column)?;
    timestamp(text).ok_or_else(|| format!("bad {column}"))
}

/// A time column's value, `None` where the catalogue leaves it empty.
pub fn optional_time(row: &Row, column: &str) -> Result<Option<Timestamp>, String> {
    match missing(field(row, column)?) {
        true => Ok(None),
        false => time(row, column).map(Some),
    }
}

/// The files write `2025-08-10T05:17Z`, without seconds.
fn timestamp(text: &str) -> Option<Timestamp> {
    Timestamp::parse(&text.trim().replace('Z', ":00Z"))
}

/// The catalogues leave a value empty, or write `-`.
fn missing(text: &str) -> bool {
    matches!(text.trim(), "" | "-")
}

fn attributes(row: &Row, structure: &[&str]) -> Result<Map<String, Value>, String> {
    let values = row
        .iter()
        .filter(|(column, value)| !structure.contains(&column.as_str()) && !missing(value));
    values
        .filter_map(|(column, value)| {
            let value = attribute_value(column, value.trim()).transpose()?;
            Some(value.map(|value| (attribute_name(column), value)))
        })
        .collect()
}

/// A time must parse. Anything else is a number, or text when it is not one: a column the
/// catalogue adds later is kept, not refused. `None` for a number that is not finite (`nan`,
/// `inf`): the catalogue has none there.
fn attribute_value(column: &str, text: &str) -> Result<Option<Value>, String> {
    let bad = || format!("{column} is {text}");
    if column.ends_with("_time") {
        let at = timestamp(text).ok_or_else(bad)?;
        return Ok(Some(Value::String(at.rfc3339().map_err(|_| bad())?)));
    }
    match text.parse::<f64>() {
        Ok(number) => Ok(serde_json::Number::from_f64(number).map(Value::Number)),
        Err(_) => Ok(Some(Value::String(text.to_owned()))),
    }
}

fn attribute_name(column: &str) -> String {
    let unit = UNITS.iter().find(|(at, part, _)| matches(at, part, column));
    match unit {
        Some((_, _, unit)) if !column.ends_with("_time") => format!("{column}_{unit}"),
        _ => column.to_owned(),
    }
}

pub fn observer_name(observer: &str) -> &str {
    OBSERVERS
        .iter()
        .find(|(raw, _)| *raw == observer)
        .map_or(observer, |(_, name)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "https://helioforecast.space/icmecat";

    #[test]
    fn a_link_to_another_host_is_refused() {
        let foreign = "<a href=\"https://example.org/HELIO4CAST_ICMECAT_v24.csv\">csv</a>";
        let insecure = "<a href=\"http://helioforecast.space/HELIO4CAST_ICMECAT_v24.csv\">csv</a>";

        for page in [foreign, insecure] {
            let error = csv_url(PAGE, page, "ICMECAT").unwrap_err();
            assert!(error.contains("helioforecast.space"));
        }
    }

    #[test]
    fn a_relative_link_stays_on_the_site() {
        let page = "<a href=\"static/sync/icmecat/HELIO4CAST_ICMECAT_v24.csv\">csv</a>";

        assert_eq!(
            csv_url(PAGE, page, "ICMECAT").unwrap(),
            "https://helioforecast.space/static/sync/icmecat/HELIO4CAST_ICMECAT_v24.csv"
        );
    }

    #[test]
    fn a_column_gets_the_unit_its_name_says() {
        let named = |column| attribute_name(column);

        assert_eq!(
            named("target_arrival_time_err"),
            "target_arrival_time_err_h"
        );
        assert_eq!(named("sse_speed_err"), "sse_speed_err_km_s");
        assert_eq!(named("sc_heeq_lon"), "sc_heeq_lon_deg");
        assert_eq!(named("hss_vtmax"), "hss_vtmax_km_s");
        assert_eq!(named("hss_btmax"), "hss_btmax_nt");
        assert_eq!(named("hss_vtmax_time"), "hss_vtmax_time");
        assert_eq!(named("sse_launch_time"), "sse_launch_time");
        assert_eq!(named("event"), "event");
    }

    #[test]
    fn empty_dash_and_blank_are_missing() {
        assert!(["", "-", " "].into_iter().all(missing));
        assert!(!missing("0"));
    }

    #[test]
    fn a_column_that_only_contains_a_unit_word_gets_no_unit() {
        for column in [
            "abstract",
            "mo_xb_flag",
            "spa_n",
            "beta_delta_x",
            "speedy",
            "vtx",
        ] {
            assert_eq!(attribute_name(column), column);
        }
    }

    #[test]
    fn a_value_that_is_no_number_is_kept_as_text_but_a_bad_time_fails() {
        assert_eq!(
            attribute_value("flag", "N15E20").unwrap(),
            Some(Value::String("N15E20".to_owned()))
        );
        assert!(attribute_value("some_time", "later").is_err());
    }

    #[test]
    fn a_column_the_catalogue_adds_later_is_read() {
        let row: Row = [("new_note", "halo"), ("speed", "400")]
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .into();

        let attributes = attributes(&row, &[]).unwrap();

        assert_eq!(attributes["new_note"], "halo");
        assert_eq!(attributes["speed_km_s"], 400.0);
    }
}
