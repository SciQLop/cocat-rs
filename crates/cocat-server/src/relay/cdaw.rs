//! CDAW, the NASA GSFC Coronal Mass Ejection Data Center: the SOHO/LASCO CME catalogue and
//! the list of type II radio bursts seen by Wind/WAVES and STEREO/WAVES.
//!
//! Both lists are plain text. LASCO's is a file with one CME per line; the burst list is an
//! HTML page holding one `<pre>` block, its links flattened to their text. Both are read by
//! splitting each line on whitespace: only the last column, the remarks, contains spaces, and
//! it comes after the fixed ones. A line the lists cannot be read from is an error, not a
//! skipped row: a layout change must fail the run rather than silently shrink the catalogue.

use async_trait::async_trait;
use cocat_core::model::EventRecord;
use cocat_core::time::Timestamp;
use scraper::{Html, Selector};
use serde_json::{Map, Value};
use time::macros::format_description;
use time::{Date, Month};
use uuid::Uuid;

use super::{Fetched, Provider};

pub struct Cdaw;

struct List {
    key: &'static str,
    name: &'static str,
    description: &'static str,
    url: &'static str,
    data_url: &'static str,
}

const LASCO: List = List {
    key: "lasco",
    name: "SOHO LASCO CMEs",
    description: "Coronal mass ejections seen by SOHO/LASCO, from the CDAW CME catalogue, https://cdaw.gsfc.nasa.gov/CME_list/",
    url: "https://cdaw.gsfc.nasa.gov/CME_list/",
    data_url: "https://cdaw.gsfc.nasa.gov/CME_list/UNIVERSAL_ver2/text_ver/univ_all.txt",
};

const TYPE_II: List = List {
    key: "type2",
    name: "WAVES type II bursts",
    description: "Decameter-hectometric type II radio bursts seen by Wind/WAVES and STEREO/WAVES, with their CMEs, from the CDAW catalogue, https://cdaw.gsfc.nasa.gov/CME_list/radio/waves_type2_description.htm",
    url: "https://cdaw.gsfc.nasa.gov/CME_list/radio/waves_type2.html",
    data_url: "https://cdaw.gsfc.nasa.gov/CME_list/radio/waves_type2.html",
};

const NAMESPACE: Uuid = Uuid::from_u128(0x3a9d41c2_7be5_4f08_b6a1_c0de5d4a7e21);

/// LASCO columns after date, time and central angle: the token index and the attribute.
const LASCO_MEASURES: [(usize, &str); 9] = [
    (3, "width_deg"),
    (4, "linear_speed_km_s"),
    (5, "speed_initial_km_s"),
    (6, "speed_final_km_s"),
    (7, "speed_20rs_km_s"),
    (8, "accel_m_s2"),
    (9, "mass_g"),
    (10, "kinetic_energy_erg"),
    (11, "mpa_deg"),
];
const LASCO_COLUMNS: usize = 12;
/// How the list writes frequencies it did not determine; such a row has a day, not a burst
/// time: its 00:00 times are placeholders.
const UNDETERMINED: &str = "????";
const DAY_SECONDS: i64 = 86_400;
/// Date, time, end date, end time, two frequencies, flare, region, importance, CME date,
/// time, angle, width, speed, plot link.
const TYPE_II_COLUMNS: usize = 15;

fn parse_lasco(body: &str) -> Result<Vec<EventRecord>, String> {
    let rows = rows(body.lines());
    let events = rows
        .iter()
        .map(|row| lasco_event(row))
        .collect::<Result<Vec<_>, _>>()?;
    non_empty(with_occurrences(LASCO.key, events))
}

fn lasco_event(row: &Row) -> Result<(String, EventRecord), String> {
    let t = &row.tokens;
    if t.len() < LASCO_COLUMNS {
        return Err(format!("LASCO line has too few columns: {}", row.line));
    }
    let start = stamp(row.date, seconds_of(t[1])?)?;
    let mut record = EventRecord::new(String::new(), start, start);
    record.attributes = numbers(t, &LASCO_MEASURES);
    insert(&mut record.attributes, "central_pa_deg", number(t[2]));
    insert(
        &mut record.attributes,
        "remarks",
        text(&t[LASCO_COLUMNS..].join(" ")),
    );
    Ok((format!("{} {}", t[0], t[1]), record))
}

fn parse_type_ii(page: &str) -> Result<Vec<EventRecord>, String> {
    let block = preformatted(page)?;
    let rows = rows(block.lines());
    let events = rows
        .iter()
        .map(|row| type_ii_event(row))
        .collect::<Result<Vec<_>, _>>()?;
    non_empty(with_occurrences(TYPE_II.key, events))
}

fn type_ii_event(row: &Row) -> Result<(String, EventRecord), String> {
    let t = &row.tokens;
    if t.len() < TYPE_II_COLUMNS {
        return Err(format!("type II line has too few columns: {}", row.line));
    }
    let time_unknown = t[4] == UNDETERMINED;
    let (start, stop) = match time_unknown {
        true => (stamp(row.date, 0)?, stamp(row.date, DAY_SECONDS)?),
        false => (
            stamp(row.date, minutes_of(t[1])? * 60)?,
            stamp(day_after_or_same(row.date, t[2])?, minutes_of(t[3])? * 60)?,
        ),
    };
    let mut record = EventRecord::new(String::new(), start, stop);
    let a = &mut record.attributes;
    if time_unknown {
        a.insert("time_unknown".into(), Value::Bool(true));
    }
    insert(a, "freq_start_khz", number(t[4]));
    insert(a, "freq_end_khz", number(t[5]));
    insert(a, "flare_location", text(t[6]));
    insert(a, "noaa_ar", number(t[7]).or_else(|| text(t[7])));
    insert(a, "importance", text(t[8].trim_end_matches('.')));
    insert(
        a,
        "cme_date_time",
        cme_time(start, t[9], t[10]).and_then(|at| Some(Value::String(at.rfc3339().ok()?))),
    );
    insert(a, "cme_cpa_deg", number(t[11]));
    insert(a, "cme_width_deg", number(t[12].trim_start_matches('>')));
    if t[12].starts_with('>') {
        a.insert("cme_width_is_lower_limit".into(), Value::Bool(true));
    }
    insert(a, "cme_speed_km_s", number(t[13]));
    insert(a, "remarks", text(&t[TYPE_II_COLUMNS..].join(" ")));
    Ok((format!("{} {}", t[0], t[1]), record))
}

/// A line of a list that starts with a date, split into its columns.
struct Row<'a> {
    line: &'a str,
    date: Date,
    tokens: Vec<&'a str>,
}

fn rows<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<Row<'a>> {
    lines
        .filter_map(|line| {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            let date =
                Date::parse(tokens.first()?, format_description!("[year]/[month]/[day]")).ok()?;
            Some(Row { line, date, tokens })
        })
        .collect()
}

fn preformatted(page: &str) -> Result<String, String> {
    let selector = Selector::parse("pre").map_err(|error| error.to_string())?;
    Ok(Html::parse_document(page)
        .select(&selector)
        .flat_map(|block| block.text())
        .collect())
}

fn non_empty(events: Vec<EventRecord>) -> Result<Vec<EventRecord>, String> {
    if events.is_empty() {
        return Err("no event found, has the page layout changed?".to_owned());
    }
    Ok(events)
}

/// Two events may start the same second; the nth one gets `#n` so that each keeps its uuid
/// as long as the list keeps its order.
fn with_occurrences(list: &str, events: Vec<(String, EventRecord)>) -> Vec<EventRecord> {
    let mut seen = std::collections::HashMap::<String, usize>::new();
    events
        .into_iter()
        .map(|(key, mut record)| {
            let nth = seen.entry(key.clone()).or_default();
            let key = if *nth == 0 {
                key
            } else {
                format!("{key}#{nth}")
            };
            *nth += 1;
            record.uuid = event_uuid(list, &key);
            record
        })
        .collect()
}

fn event_uuid(list: &str, key: &str) -> String {
    Uuid::new_v5(&NAMESPACE, format!("{list}/{key}").as_bytes()).to_string()
}

fn stamp(date: Date, seconds: i64) -> Result<Timestamp, String> {
    let midnight = date.midnight().assume_utc().unix_timestamp();
    Timestamp::from_unix_micros(i128::from(midnight + seconds) * 1_000_000)
        .ok_or_else(|| format!("{date} is out of range"))
}

fn seconds_of(clock: &str) -> Result<i64, String> {
    let mut parts = clock.split(':').map(str::parse::<i64>);
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(Ok(h)), Some(Ok(m)), Some(Ok(s)), None) => Ok(h * 3600 + m * 60 + s),
        _ => Err(format!("{clock} is not a time")),
    }
}

/// `24:00` is how the list writes the end of a day.
fn minutes_of(clock: &str) -> Result<i64, String> {
    let (hours, minutes) = clock
        .split_once(':')
        .ok_or_else(|| format!("{clock} is not a time"))?;
    let parse = |part: &str| {
        part.parse::<i64>()
            .map_err(|_| format!("{clock} is not a time"))
    };
    Ok(parse(hours)? * 60 + parse(minutes)?)
}

fn month_day(text: &str) -> Option<(u8, u8)> {
    let (month, day) = text.split_once('/')?;
    Some((month.parse().ok()?, day.parse().ok()?))
}

fn date_in(year: i32, (month, day): (u8, u8)) -> Option<Date> {
    Date::from_calendar_date(year, Month::try_from(month).ok()?, day).ok()
}

/// The burst's end has no year: it is the start's, or the next one for a burst running past
/// New Year.
fn day_after_or_same(start: Date, end: &str) -> Result<Date, String> {
    let end_day = month_day(end).ok_or_else(|| format!("{end} is not a date"))?;
    [start.year(), start.year() + 1]
        .into_iter()
        .filter_map(|year| date_in(year, end_day))
        .find(|date| *date >= start)
        .ok_or_else(|| format!("{end} is not a date after {start}"))
}

/// The CME has no year either and may come just before or after the burst: it is the nearest
/// to the burst's start.
fn cme_time(burst: Timestamp, day: &str, clock: &str) -> Option<Timestamp> {
    let day = month_day(day)?;
    let seconds = minutes_of(clock).ok()? * 60;
    let year = burst.parts().year;
    (year - 1..=year + 1)
        .filter_map(|year| stamp(date_in(year, day)?, seconds).ok())
        .min_by_key(|at| (at.unix_micros() - burst.unix_micros()).abs())
}

fn numbers(tokens: &[&str], columns: &[(usize, &str)]) -> Map<String, Value> {
    let mut map = Map::new();
    for (index, name) in columns {
        insert(&mut map, name, number(tokens[*index]));
    }
    map
}

/// The lists mark a figure that is a rough estimate with a trailing `*`, and one they do not
/// have with dashes or `????`; anything else not a number, like `Halo`, is no number either.
fn number(token: &str) -> Option<Value> {
    let figure = token.trim_end_matches('*');
    figure.parse::<i64>().map(Value::from).ok().or_else(|| {
        let float: f64 = figure.parse().ok()?;
        serde_json::Number::from_f64(float).map(Value::Number)
    })
}

fn text(token: &str) -> Option<Value> {
    let missing = token.is_empty() || token.chars().all(|c| matches!(c, '-' | '?' | '/' | ':'));
    (!missing).then(|| Value::String(token.to_owned()))
}

fn insert(map: &mut Map<String, Value>, name: &str, value: Option<Value>) {
    if let Some(value) = value {
        map.insert(name.to_owned(), value);
    }
}

fn catalogue(list: &List, events: Vec<EventRecord>) -> Fetched {
    Fetched {
        key: list.key.to_owned(),
        folder: "CDAW".to_owned(),
        name: list.name.to_owned(),
        description: list.description.to_owned(),
        url: list.url.to_owned(),
        catalogue: Uuid::new_v5(&NAMESPACE, list.key.as_bytes()).to_string(),
        columns: Vec::new(),
        events: Ok(events),
    }
}

async fn download(http: &reqwest::Client, list: &List) -> Result<String, String> {
    let describe = |error: reqwest::Error| format!("{}: {}", list.name, error.without_url());
    let body = http
        .get(list.data_url)
        .send()
        .await
        .map_err(describe)?
        .error_for_status()
        .map_err(describe)?
        .bytes()
        .await
        .map_err(describe)?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

async fn fetch_list(
    http: &reqwest::Client,
    list: &List,
    parse: fn(&str) -> Result<Vec<EventRecord>, String>,
) -> Result<Fetched, String> {
    let body = download(http, list).await?;
    let events = parse(&body).map_err(|error| format!("{}: {error}", list.name))?;
    Ok(catalogue(list, events))
}

#[async_trait]
impl Provider for Cdaw {
    fn id(&self) -> &'static str {
        "cdaw"
    }

    fn source(&self) -> &'static str {
        "CDAW (NASA GSFC)"
    }

    /// The LASCO list is updated weekly, on Fridays; the run follows on Monday.
    fn default_schedule(&self) -> &'static str {
        "30 5 * * 1"
    }

    async fn fetch(
        &self,
        http: &reqwest::Client,
        _settings: &Value,
        _last_ok: Option<Timestamp>,
    ) -> Result<Vec<Fetched>, String> {
        Ok(vec![
            fetch_list(http, &LASCO, parse_lasco).await?,
            fetch_list(http, &TYPE_II, parse_type_ii).await?,
        ])
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const LASCO_FIXTURE: &str = include_str!("../../tests/fixtures/relay/cdaw_univ_all_head.txt");
    const TYPE_II_FIXTURE: &str = include_str!("../../tests/fixtures/relay/cdaw_waves_type2.html");

    fn at(text: &str) -> Timestamp {
        Timestamp::parse(text).unwrap()
    }

    fn lasco_at(start: &str) -> Vec<EventRecord> {
        let start = at(start);
        parse_lasco(LASCO_FIXTURE)
            .unwrap()
            .into_iter()
            .filter(|event| event.start == start)
            .collect()
    }

    fn burst_at(start: &str) -> EventRecord {
        let start = at(start);
        let bursts = parse_type_ii(TYPE_II_FIXTURE).unwrap();
        bursts
            .into_iter()
            .find(|event| event.start == start)
            .unwrap()
    }

    #[test]
    fn a_lasco_line_becomes_an_event_with_its_measures() {
        let cme = &lasco_at("1996-01-22T03:11:01Z")[0];

        assert_eq!(cme.uuid, event_uuid("lasco", "1996/01/22 03:11:01"));
        assert_eq!(cme.stop, cme.start);
        let expected = json!({
            "central_pa_deg": 105, "width_deg": 37, "linear_speed_km_s": 267,
            "speed_initial_km_s": 401, "speed_final_km_s": 130, "speed_20rs_km_s": 0,
            "accel_m_s2": -126.3, "mass_g": 7.1e13, "kinetic_energy_erg": 2.5e28,
            "mpa_deg": 103, "remarks": "Very Poor Event; Only C3",
        });
        assert_eq!(Value::Object(cme.attributes.clone()), expected);
    }

    #[test]
    fn a_halo_cme_is_360_degrees_wide() {
        let halo = &lasco_at("1996-04-29T14:38:48Z")[0];

        assert_eq!(halo.attributes["width_deg"], json!(360));
        assert!(!halo.attributes.contains_key("central_pa_deg"));
        assert_eq!(halo.attributes["mpa_deg"], json!(149));
        assert_eq!(
            halo.attributes["remarks"],
            json!("Faint; Very Poor Event; Only 2 points; Only C2")
        );
    }

    #[test]
    fn missing_values_are_left_out() {
        let halo = &lasco_at("1996-04-29T14:38:48Z")[0];

        let left_out = [
            "speed_initial_km_s",
            "speed_final_km_s",
            "speed_20rs_km_s",
            "accel_m_s2",
            "mass_g",
            "kinetic_energy_erg",
        ];
        for name in left_out {
            assert!(!halo.attributes.contains_key(name), "{name}");
        }
        assert_eq!(halo.attributes["linear_speed_km_s"], json!(65));
    }

    #[test]
    fn a_trailing_star_marks_an_estimate_not_part_of_the_number() {
        let cme = &lasco_at("1996-01-11T00:14:36Z")[0];

        assert_eq!(cme.attributes["accel_m_s2"], json!(-64.3));
        assert!(!cme.attributes.contains_key("mass_g"));
    }

    #[test]
    fn two_cmes_starting_the_same_second_keep_their_own_uuids() {
        let pair = lasco_at("1996-01-30T05:56:07Z");

        assert_eq!(pair.len(), 2);
        assert_ne!(pair[0].uuid, pair[1].uuid);
        assert_eq!(pair[0].uuid, event_uuid("lasco", "1996/01/30 05:56:07"));
        assert_eq!(pair[1].uuid, event_uuid("lasco", "1996/01/30 05:56:07#1"));
        assert_eq!(pair[0].attributes["central_pa_deg"], json!(79));
        assert_eq!(pair[1].attributes["central_pa_deg"], json!(272));
    }

    #[test]
    fn a_type_ii_row_spans_its_burst() {
        let burst = burst_at("1997-04-01T14:00:00Z");

        assert_eq!(burst.uuid, event_uuid("type2", "1997/04/01 14:00"));
        assert_eq!(burst.stop, at("1997-04-01T14:15:00Z"));
        let expected = json!({
            "freq_start_khz": 8000, "freq_end_khz": 4000, "flare_location": "S25E16",
            "noaa_ar": 8026, "importance": "M1.3", "cme_date_time": "1997-04-01T15:18:00Z",
            "cme_cpa_deg": 74, "cme_width_deg": 79, "cme_speed_km_s": 312,
        });
        assert_eq!(Value::Object(burst.attributes), expected);
    }

    #[test]
    fn a_burst_ending_the_next_day_ends_the_next_day() {
        let burst = burst_at("1997-05-12T05:15:00Z");
        assert_eq!(burst.stop, at("1997-05-14T16:00:00Z"));

        let over_new_year = burst_at("2023-12-31T21:57:00Z");
        assert_eq!(over_new_year.stop, at("2024-01-01T00:50:00Z"));
        assert_eq!(
            over_new_year.attributes["cme_date_time"],
            json!("2023-12-31T22:00:00Z")
        );

        let to_midnight = burst_at("2006-12-06T19:00:00Z");
        assert_eq!(to_midnight.stop, at("2006-12-09T00:00:00Z"));
    }

    #[test]
    fn a_burst_row_with_nothing_known_keeps_only_what_is_known() {
        let burst = burst_at("1996-05-15T05:45:00Z");

        let expected =
            json!({"freq_start_khz": 10000, "freq_end_khz": 500, "remarks": "uncertain"});
        assert_eq!(Value::Object(burst.attributes), expected);
    }

    #[test]
    fn halo_cpa_and_unknown_frequencies_are_left_out() {
        let halo = burst_at("1997-05-12T05:15:00Z");
        assert!(!halo.attributes.contains_key("cme_cpa_deg"));
        assert_eq!(halo.attributes["cme_width_deg"], json!(360));

        let unmeasured = burst_at("2003-06-05T00:00:00Z");
        assert!(!unmeasured.attributes.contains_key("freq_start_khz"));
        assert_eq!(unmeasured.attributes["flare_location"], json!("W90b"));
        assert_eq!(unmeasured.attributes["noaa_ar"], json!("DIM"));
        assert!(!unmeasured.attributes.contains_key("importance"));
    }

    #[test]
    fn a_burst_with_undetermined_frequencies_spans_its_whole_day() {
        let burst = burst_at("2003-06-05T00:00:00Z");

        assert_eq!(burst.stop, at("2003-06-06T00:00:00Z"));
        assert_eq!(burst.attributes["time_unknown"], json!(true));
        assert!(
            !burst_at("2006-12-06T19:00:00Z")
                .attributes
                .contains_key("time_unknown")
        );
    }

    #[test]
    fn a_width_with_a_lower_limit_says_so() {
        let burst = burst_at("1998-05-11T21:40:00Z");

        assert_eq!(burst.attributes["cme_width_deg"], json!(301));
        assert_eq!(burst.attributes["cme_width_is_lower_limit"], json!(true));
    }

    #[test]
    fn a_cme_is_dated_in_the_year_nearest_the_burst() {
        let burst = at("2024-01-01T00:10:00Z");

        assert_eq!(
            cme_time(burst, "12/31", "23:50"),
            Some(at("2023-12-31T23:50:00Z"))
        );
        assert_eq!(
            cme_time(burst, "01/01", "00:30"),
            Some(at("2024-01-01T00:30:00Z"))
        );
    }

    #[test]
    fn a_page_or_file_without_events_is_an_error() {
        assert!(parse_lasco("<html>moved</html>").is_err());
        assert!(parse_type_ii("<html><pre>nothing</pre></html>").is_err());
    }

    #[test]
    fn a_row_cut_short_is_an_error() {
        assert!(parse_lasco("1996/01/11  00:14:36    267     18").is_err());
    }

    #[test]
    fn both_lists_land_in_the_cdaw_folder() {
        let lands = [
            catalogue(&LASCO, parse_lasco(LASCO_FIXTURE).unwrap()),
            catalogue(&TYPE_II, parse_type_ii(TYPE_II_FIXTURE).unwrap()),
        ]
        .map(|list| (list.key, list.folder, list.name));

        assert_eq!(
            lands,
            [
                ("lasco".into(), "CDAW".into(), "SOHO LASCO CMEs".into()),
                ("type2".into(), "CDAW".into(), "WAVES type II bursts".into()),
            ]
        );
    }

    #[test]
    fn the_provider_is_registered_with_no_setting() {
        let provider = crate::relay::providers()
            .into_iter()
            .find(|provider| provider.id() == "cdaw")
            .unwrap();

        assert_eq!(provider.source(), "CDAW (NASA GSFC)");
        assert!(provider.settings().is_empty());
    }
}
