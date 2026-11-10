//! DONKI, NASA CCMC's space weather database, read through api.nasa.gov: one public catalogue
//! per type of event (flares, CMEs, shocks…).
//!
//! The first run reads the whole history, a year per request. Later runs read the last 90
//! days, since DONKI revises recent events; what it holds from before that stays as it is.

use async_trait::async_trait;
use cocat_core::model::EventRecord;
use cocat_core::time::Timestamp;
use serde_json::{Map, Value};
use time::macros::{date, format_description};
use time::{Date, Duration, Month};
use uuid::Uuid;

use super::{Fetched, Provider};

pub struct Donki;

#[derive(Debug, Clone, Copy)]
struct Kind {
    code: &'static str,
    name: &'static str,
    id_field: &'static str,
    start_field: &'static str,
}

const KINDS: [Kind; 8] = [
    Kind {
        code: "FLR",
        name: "Solar flares",
        id_field: "flrID",
        start_field: "beginTime",
    },
    Kind {
        code: "CME",
        name: "Coronal mass ejections",
        id_field: "activityID",
        start_field: "startTime",
    },
    Kind {
        code: "SEP",
        name: "Solar energetic particles",
        id_field: "sepID",
        start_field: "eventTime",
    },
    Kind {
        code: "IPS",
        name: "Interplanetary shocks",
        id_field: "activityID",
        start_field: "eventTime",
    },
    Kind {
        code: "GST",
        name: "Geomagnetic storms",
        id_field: "gstID",
        start_field: "startTime",
    },
    Kind {
        code: "MPC",
        name: "Magnetopause crossings",
        id_field: "mpcID",
        start_field: "eventTime",
    },
    Kind {
        code: "RBE",
        name: "Radiation belt enhancements",
        id_field: "rbeID",
        start_field: "eventTime",
    },
    Kind {
        code: "HSS",
        name: "High speed streams",
        id_field: "hssID",
        start_field: "eventTime",
    },
];

const NAMESPACE: Uuid = Uuid::from_u128(0x6ef18c5f_f0a8_49d6_8d92_7b8c37272eaf);
const ENDPOINT: &str = "https://api.nasa.gov/DONKI";
const DEFAULT_KEY: &str = "DEMO_KEY";
const FIRST_DAY: Date = date!(2010 - 01 - 01);
const WINDOW: Duration = Duration::days(90);
/// A year per request, ends included.
const REQUEST_SPAN: Duration = Duration::days(364);
/// The fields that end an event, first to be found wins; an event with none is a point.
const STOP_FIELDS: [&str; 2] = ["endTime", "peakTime"];
/// Lists of objects joined into one text attribute: field, and the member field to read.
const JOINED: [(&str, &str); 2] = [
    ("instruments", "displayName"),
    ("linkedEvents", "activityID"),
];

fn parse(kind: &Kind, body: &[u8]) -> Result<Vec<EventRecord>, String> {
    if body.trim_ascii().is_empty() {
        return Ok(Vec::new());
    }
    let events: Vec<Value> = serde_json::from_slice(body)
        .map_err(|error| format!("{} is not a list: {error}", kind.code))?;
    events
        .iter()
        .map(|event| parse_event(kind, event).map_err(|error| format!("{}: {error}", kind.code)))
        .collect()
}

fn parse_event(kind: &Kind, event: &Value) -> Result<EventRecord, String> {
    let key =
        text(event, kind.id_field).ok_or_else(|| format!("an event has no {}", kind.id_field))?;
    let start = time_of(event, kind.start_field)
        .ok_or_else(|| format!("{key} has no {}", kind.start_field))?;
    let stop = STOP_FIELDS
        .iter()
        .find_map(|field| time_of(event, field))
        .or_else(|| kp_readings(event).filter_map(|(_, at)| at).max())
        .unwrap_or(start);
    let uuid = Uuid::new_v5(&NAMESPACE, key.as_bytes()).to_string();
    let mut record = EventRecord::new(uuid, start, stop);
    record.attributes = attributes(kind, event);
    Ok(record)
}

fn text<'a>(event: &'a Value, field: &str) -> Option<&'a str> {
    event.get(field)?.as_str()
}

/// DONKI writes `2024-05-14T16:46Z`, without seconds.
fn time_of(event: &Value, field: &str) -> Option<Timestamp> {
    let text = text(event, field)?;
    Timestamp::parse(text).or_else(|| Timestamp::parse(&text.replace('Z', ":00Z")))
}

fn attributes(kind: &Kind, event: &Value) -> Map<String, Value> {
    let fields = event.as_object().into_iter().flatten();
    let scalars = fields
        .filter(|(name, value)| {
            *name != kind.id_field
                && *name != kind.start_field
                && !value.is_array()
                && !value.is_object()
                && !value.is_null()
        })
        .map(|(name, value)| (name.clone(), value.clone()));
    let joined = JOINED.iter().filter_map(|(field, member)| {
        Some((
            field.to_string(),
            Value::String(joined(event, field, member)?),
        ))
    });
    let kp_max = kp_readings(event)
        .map(|(kp, _)| kp)
        .max_by(f64::total_cmp)
        .map(|kp| ("kp_max".to_owned(), Value::from(kp)));
    scalars.chain(joined).chain(kp_max).collect()
}

/// A storm's Kp readings: the index, and when it was observed.
fn kp_readings(event: &Value) -> impl Iterator<Item = (f64, Option<Timestamp>)> + '_ {
    let readings = event
        .get("allKpIndex")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    readings.filter_map(|reading| {
        Some((
            reading.get("kpIndex")?.as_f64()?,
            time_of(reading, "observedTime"),
        ))
    })
}

fn joined(event: &Value, field: &str, member: &str) -> Option<String> {
    let names: Vec<&str> = event
        .get(field)?
        .as_array()?
        .iter()
        .filter_map(|entry| text(entry, member))
        .collect();
    Some(names.join(", ")).filter(|names| !names.is_empty())
}

fn catalogue(kind: &Kind, events: Vec<EventRecord>) -> Fetched {
    Fetched {
        key: kind.code.to_lowercase(),
        folder: format!("DONKI/{}", kind.name),
        name: kind.name.to_owned(),
        description: format!(
            "{} from NASA CCMC's DONKI space weather database, https://ccmc.gsfc.nasa.gov/tools/DONKI/",
            kind.name
        ),
        url: format!(
            "https://kauai.ccmc.gsfc.nasa.gov/DONKI/search/#{}",
            kind.code
        ),
        catalogue: Uuid::new_v5(&NAMESPACE, kind.code.as_bytes()).to_string(),
        events,
    }
}

/// Where this run starts reading: the whole history the first time, else the window, or
/// further back when the last run is older than that.
fn first_day(last_ok: Option<Timestamp>, now: Timestamp) -> Result<Date, String> {
    let Some(last_ok) = last_ok else {
        return Ok(FIRST_DAY);
    };
    Ok(date_of(last_ok)?.min(date_of(window_start(now)?)?))
}

fn windows(last_ok: Option<Timestamp>, now: Timestamp) -> Result<Vec<(Date, Date)>, String> {
    let last_day = date_of(now)?;
    let first = first_day(last_ok, now)?;
    let spans = std::iter::successors(Some(first), |start| {
        Some(*start + REQUEST_SPAN + Duration::DAY)
    })
    .take_while(|start| *start <= last_day);
    Ok(spans
        .map(|start| (start, (start + REQUEST_SPAN).min(last_day)))
        .collect())
}

fn window_start(now: Timestamp) -> Result<Timestamp, String> {
    let micros = now.unix_micros() - WINDOW.whole_microseconds();
    Timestamp::from_unix_micros(micros).ok_or_else(|| "the window starts out of range".to_owned())
}

fn date_of(at: Timestamp) -> Result<Date, String> {
    let parts = at.parts();
    let month = Month::try_from(parts.month).map_err(|error| error.to_string())?;
    Date::from_calendar_date(parts.year, month, parts.day).map_err(|error| error.to_string())
}

fn request_url(
    kind: &Kind,
    (start, end): (Date, Date),
    settings: &Value,
) -> Result<String, String> {
    let day = |date: Date| {
        date.format(format_description!("[year]-[month]-[day]"))
            .map_err(|error| error.to_string())
    };
    let key = text(settings, "api_key")
        .filter(|key| !key.is_empty())
        .unwrap_or(DEFAULT_KEY);
    let params = [
        ("startDate", day(start)?),
        ("endDate", day(end)?),
        ("api_key", key.to_owned()),
    ];
    reqwest::Url::parse_with_params(&format!("{ENDPOINT}/{}", kind.code), params)
        .map(String::from)
        .map_err(|error| error.to_string())
}

/// Reqwest errors carry the URL, and the URL carries the key.
fn describe(error: reqwest::Error) -> String {
    error.without_url().to_string()
}

async fn fetch_window(http: &reqwest::Client, url: String) -> Result<Vec<u8>, String> {
    let response = http.get(url).send().await.map_err(describe)?;
    let body = response
        .error_for_status()
        .map_err(describe)?
        .bytes()
        .await
        .map_err(describe)?;
    Ok(body.to_vec())
}

async fn fetch_kind(
    http: &reqwest::Client,
    settings: &Value,
    windows: &[(Date, Date)],
    kind: &Kind,
) -> Result<Fetched, String> {
    let mut events = Vec::new();
    for window in windows {
        let url = request_url(kind, *window, settings)?;
        let body = fetch_window(http, url)
            .await
            .map_err(|error| format!("{} {}: {error}", kind.code, window.0))?;
        events.extend(parse(kind, &body)?);
    }
    Ok(catalogue(kind, events))
}

#[async_trait]
impl Provider for Donki {
    fn id(&self) -> &'static str {
        "donki"
    }

    fn source(&self) -> &'static str {
        "DONKI (NASA CCMC)"
    }

    fn default_schedule(&self) -> &'static str {
        "30 3 * * *"
    }

    fn settings(&self) -> &'static [&'static str] {
        &["api_key"]
    }

    async fn fetch(
        &self,
        http: &reqwest::Client,
        settings: &Value,
        last_ok: Option<Timestamp>,
    ) -> Result<Vec<Fetched>, String> {
        let windows = windows(last_ok, Timestamp::now())?;
        let mut lists = Vec::new();
        for kind in &KINDS {
            lists.push(fetch_kind(http, settings, &windows, kind).await?);
        }
        Ok(lists)
    }

    /// Whatever is older than the window was not asked for, so its absence says nothing.
    fn keep(&self, event: &EventRecord) -> bool {
        window_start(Timestamp::now()).is_ok_and(|window| event.start < window)
    }
}

#[cfg(test)]
mod tests {
    use cocat_core::model::{Status, Wanted, event, hold};
    use serde_json::json;
    use yrs::{Doc, Transact};

    use super::*;

    fn fixture(code: &str) -> &'static [u8] {
        match code {
            "FLR" => include_bytes!("../../tests/fixtures/relay/donki_FLR.json"),
            "CME" => include_bytes!("../../tests/fixtures/relay/donki_CME.json"),
            "SEP" => include_bytes!("../../tests/fixtures/relay/donki_SEP.json"),
            "IPS" => include_bytes!("../../tests/fixtures/relay/donki_IPS.json"),
            "GST" => include_bytes!("../../tests/fixtures/relay/donki_GST.json"),
            "MPC" => include_bytes!("../../tests/fixtures/relay/donki_MPC.json"),
            "RBE" => include_bytes!("../../tests/fixtures/relay/donki_RBE.json"),
            _ => include_bytes!("../../tests/fixtures/relay/donki_HSS.json"),
        }
    }

    fn at(text: &str) -> Timestamp {
        Timestamp::parse(text).unwrap()
    }

    fn kind(code: &str) -> Kind {
        KINDS.into_iter().find(|kind| kind.code == code).unwrap()
    }

    fn flares() -> Vec<EventRecord> {
        parse(&kind("FLR"), fixture("FLR")).unwrap()
    }

    #[test]
    fn donki_flares_become_events_keyed_by_their_id() {
        let flares = flares();

        assert_eq!(flares.len(), 2);
        let flare = &flares[0];
        let key = "2024-05-14T16:46:00-FLR-001";
        assert_eq!(
            flare.uuid,
            Uuid::new_v5(&NAMESPACE, key.as_bytes()).to_string()
        );
        assert_eq!(flare.start, at("2024-05-14T16:46:00Z"));
        assert_eq!(flare.stop, at("2024-05-14T17:08:00Z"));
        assert_eq!(flare.attributes["classType"], json!("X8.7"));
        assert_eq!(flare.attributes["activeRegionNum"], json!(13664));
        assert_eq!(
            flare.attributes["instruments"],
            json!("GOES-P: EXIS 1.0-8.0")
        );
        assert_eq!(
            flare.attributes["linkedEvents"],
            json!("2024-05-14T17:12:00-CME-001")
        );
        assert!(!flare.attributes.contains_key("flrID"));
        assert!(!flare.attributes.contains_key("beginTime"));
    }

    #[test]
    fn a_flare_without_an_end_stops_at_its_peak() {
        let flare = &flares()[1];

        assert_eq!(flare.start, at("2024-05-15T08:30:00Z"));
        assert_eq!(flare.stop, at("2024-05-15T08:41:00Z"));
        assert!(!flare.attributes.contains_key("linkedEvents"));
    }

    #[test]
    fn a_shock_is_a_point_event_with_its_linked_events() {
        let shock = &parse(&kind("IPS"), fixture("IPS")).unwrap()[0];

        assert_eq!(shock.start, at("2024-05-02T07:46:00Z"));
        assert_eq!(shock.stop, shock.start);
        assert_eq!(
            shock.attributes["linkedEvents"],
            json!(
                "2024-05-02T13:15:00-HSS-001, 2024-05-02T15:00:00-GST-001, 2024-05-02T15:49:00-MPC-001"
            )
        );
    }

    #[test]
    fn each_type_lands_in_its_catalogue_under_donki() {
        let lands: Vec<_> = KINDS
            .into_iter()
            .map(|kind| {
                let events = parse(&kind, fixture(kind.code)).unwrap();
                assert!(!events.is_empty(), "{}", kind.code);
                let list = catalogue(&kind, events);
                (list.key, list.folder, list.name, list.events.is_empty())
            })
            .collect();

        let expected = [
            ("flr", "Solar flares"),
            ("cme", "Coronal mass ejections"),
            ("sep", "Solar energetic particles"),
            ("ips", "Interplanetary shocks"),
            ("gst", "Geomagnetic storms"),
            ("mpc", "Magnetopause crossings"),
            ("rbe", "Radiation belt enhancements"),
            ("hss", "High speed streams"),
        ];
        let expected: Vec<_> = expected
            .into_iter()
            .map(|(key, name)| {
                (
                    key.to_owned(),
                    format!("DONKI/{name}"),
                    name.to_owned(),
                    false,
                )
            })
            .collect();
        assert_eq!(lands, expected);
    }

    #[test]
    fn the_provider_is_registered_off_with_its_key_setting() {
        let provider = crate::relay::providers()
            .into_iter()
            .find(|provider| provider.id() == "donki")
            .unwrap();

        assert_eq!(provider.source(), "DONKI (NASA CCMC)");
        assert_eq!(provider.settings(), ["api_key"]);
    }

    #[test]
    fn the_key_comes_from_the_settings() {
        let window = (at("2024-05-01T00:00:00Z"), at("2024-05-31T00:00:00Z"));
        let window = (date_of(window.0).unwrap(), date_of(window.1).unwrap());
        let url = |settings| request_url(&kind("FLR"), window, &settings).unwrap();

        assert_eq!(
            url(json!({"api_key": "k3y"})),
            "https://api.nasa.gov/DONKI/FLR?startDate=2024-05-01&endDate=2024-05-31&api_key=k3y"
        );
        assert!(url(json!({})).ends_with("api_key=DEMO_KEY"));
        assert!(url(json!({"api_key": ""})).ends_with("api_key=DEMO_KEY"));
    }

    #[test]
    fn the_first_run_asks_year_by_year_from_2010() {
        let now = at("2026-11-10T12:00:00Z");

        let windows = windows(None, now).unwrap();

        assert_eq!(windows.len(), 17);
        assert_eq!(windows[0].0.to_string(), "2010-01-01");
        assert_eq!(windows[1].0.to_string(), "2011-01-01");
        assert_eq!(windows[16].1.to_string(), "2026-11-10");
    }

    #[test]
    fn a_later_run_asks_for_the_last_90_days() {
        let now = at("2026-11-10T12:00:00Z");

        let windows = windows(Some(at("2026-11-09T03:30:00Z")), now).unwrap();

        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].0.to_string(), "2026-08-12");
        assert_eq!(windows[0].1.to_string(), "2026-11-10");
    }

    #[test]
    fn a_run_after_a_long_pause_starts_where_the_last_one_ended() {
        let windows =
            windows(Some(at("2026-01-15T03:30:00Z")), at("2026-11-10T12:00:00Z")).unwrap();

        assert_eq!(windows[0].0.to_string(), "2026-01-15");
    }

    #[test]
    fn events_before_the_window_are_kept() {
        let (old, recent) = (at("2020-01-01T00:00:00Z"), Timestamp::now());
        let (old_uuid, recent_uuid) = ("old", "recent");
        let record = |uuid, start, class| {
            let mut record = EventRecord::new(uuid, start, start);
            record.attributes.insert("classType".into(), json!(class));
            record
        };
        let doc = Doc::new();
        let wanted = |events| Wanted {
            catalogue: "c".into(),
            name: "Solar flares".into(),
            events,
        };
        hold(
            &mut doc.transact_mut(),
            &wanted(vec![
                record(old_uuid, old, "M1"),
                record(recent_uuid, recent, "M1"),
            ]),
            |_| false,
        );

        let counts = hold(
            &mut doc.transact_mut(),
            &wanted(vec![record(recent_uuid, recent, "X2")]),
            |event| Donki.keep(event),
        );

        assert_eq!((counts.changed, counts.removed), (1, 0));
        let txn = doc.transact();
        assert_eq!(event(&txn, old_uuid).unwrap().status, Status::Confirmed);
        assert_eq!(
            event(&txn, recent_uuid).unwrap().attributes["classType"],
            json!("X2")
        );
        assert!(Donki.keep(&record(old_uuid, old, "M1")));
        assert!(!Donki.keep(&record(recent_uuid, recent, "M1")));
    }

    #[test]
    fn an_empty_answer_is_no_events() {
        for body in ["", " \n"] {
            assert_eq!(parse(&kind("FLR"), body.as_bytes()), Ok(Vec::new()));
        }
    }

    #[test]
    fn a_storm_lasts_until_its_last_kp_reading_and_keeps_its_peak() {
        let storms = parse(&kind("GST"), fixture("GST")).unwrap();

        assert_eq!(storms[0].start, at("2024-05-02T15:00:00Z"));
        assert_eq!(storms[0].stop, at("2024-05-02T21:00:00Z"));
        assert_eq!(storms[0].attributes["kp_max"], json!(6.67));
        assert_eq!(storms[1].stop, at("2024-05-12T06:00:00Z"));
        assert_eq!(storms[1].attributes["kp_max"], json!(9.0));
    }
}
