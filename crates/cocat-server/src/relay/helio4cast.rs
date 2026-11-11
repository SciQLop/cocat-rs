//! The other HELIO4CAST catalogues (Möstl et al., helioforecast.space), next to the ICMECAT:
//! ARRCAT, the modeled arrivals of CMEs seen by the STEREO heliospheric imagers, one catalogue
//! per target; SIRCAT, the stream interaction regions, one per spacecraft; and LineupCAT, the
//! multi-spacecraft lineups, one catalogue. Each is a CSV linked from its page, see `catcsv`.
//!
//! An ARRCAT or LineupCAT event is a point, its start and stop equal, like a DONKI shock.

use std::collections::HashMap;

use async_trait::async_trait;
use cocat_core::model::EventRecord;
use cocat_core::time::Timestamp;
use serde_json::Value;
use uuid::Uuid;

use super::catcsv::{self, Entry, Row};
use super::{Fetched, Provider};

pub struct Helio4cast;

const NAMESPACE: Uuid = Uuid::from_u128(0x3b1f6d82_74ac_4e09_9a5d_c2e80f17b6a4);
const CREDIT: &str = "If results from this catalogue go into a peer-reviewed paper, the authors ask to be contacted about co-authorship.";

/// What tells one catalogue from the others: where it is, how its rows become events, and how
/// its lists are described.
struct Catalogue {
    name: &'static str,
    page: &'static str,
    tag: &'static str,
    structure: &'static [&'static str],
    entry: fn(&Row) -> Result<Entry, String>,
    describe: fn(&str) -> String,
}

const CATALOGUES: [Catalogue; 3] = [
    Catalogue {
        name: "ARRCAT",
        page: "https://helioforecast.space/arrcat",
        tag: "ARRCAT",
        structure: &["", "id", "target_name", "target_arrival_time"],
        entry: arrival,
        describe: arrivals_of,
    },
    Catalogue {
        name: "SIRCAT",
        page: "https://helioforecast.space/sircat",
        tag: "SIRCAT",
        structure: &["", "sircat_id", "sc_insitu", "sir_start_time"],
        entry: stream,
        describe: streams_seen_by,
    },
    Catalogue {
        name: "LineupCAT",
        page: "https://helioforecast.space/lineups",
        tag: "multipoint",
        structure: &["", "event_start_time"],
        entry: lineup,
        describe: lineups_of,
    },
];

/// A point event: its key says the catalogue's id, and the target it is seen at.
fn arrival(row: &Row) -> Result<Entry, String> {
    let target = catcsv::field(row, "target_name")?;
    let key = format!("{}:{target}", catcsv::field(row, "id")?);
    let at = catcsv::named(&key, catcsv::time(row, "target_arrival_time"))?;
    Ok(Entry {
        list: target.to_owned(),
        start: at,
        stop: at,
        key,
    })
}

/// The catalogue gives no SIR times for Wind, only those of its high speed streams.
fn stream(row: &Row) -> Result<Entry, String> {
    let key = catcsv::field(row, "sircat_id")?.to_owned();
    let time = |column| catcsv::named(&key, catcsv::optional_time(row, column));
    let start = time("sir_start_time")?.or(time("hss_start_time")?);
    let start = start.ok_or_else(|| format!("{key}: no start time"))?;
    let stop = [time("sir_end_time")?, time("hss_end_time")?]
        .into_iter()
        .flatten()
        .fold(start, Timestamp::max);
    Ok(Entry {
        list: catcsv::field(row, "sc_insitu")?.to_owned(),
        key,
        start,
        stop,
    })
}

// simplify: a renumbered catalog_id replaces its event, there is no stabler id in the file.
/// Several rows of one lineup can share a spacecraft: the row's own id in the other
/// catalogues tells them apart.
fn lineup(row: &Row) -> Result<Entry, String> {
    let key = ["lineupcat_id", "spacecraft", "catalog_id"]
        .map(|column| catcsv::field(row, column))
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?
        .join(":");
    let at = catcsv::named(&key, catcsv::time(row, "event_start_time"))?;
    Ok(Entry {
        list: String::new(),
        start: at,
        stop: at,
        key,
    })
}

fn arrivals_of(target: &str) -> String {
    format!(
        "Arrivals of coronal mass ejections at {target}, modeled with SSEF30 from the CMEs the STEREO heliospheric imagers tracked (HIGeoCAT, RAL Space). From the HELIO4CAST ARRCAT 2.0 (Moestl et al.), https://helioforecast.space/arrcat. {CREDIT} They also acknowledge the use of the spiceypy python package."
    )
}

fn streams_seen_by(spacecraft: &str) -> String {
    format!(
        "Stream interaction regions and high speed streams seen in situ by {spacecraft}, merged from the catalogues of Jian et al., Grandin et al. and Huang et al. From the HELIO4CAST SIRCAT 1.0 (Moestl et al.), https://helioforecast.space/sircat. {CREDIT}"
    )
}

fn lineups_of(name: &str) -> String {
    format!(
        "Multi-spacecraft lineups: CMEs seen by several spacecraft, in situ or by imagers, one event per observation, grouped by lineupcat_id; catalog_id names the entry in the ICMECAT, HIGeoCAT or DONKI. The {name} v3.0 of the HELIO4CAST catalogue (Weiler, Moestl, Davies et al.), https://helioforecast.space/lineups. {CREDIT}"
    )
}

fn lists(catalogue: &Catalogue, body: &str) -> Result<Vec<Fetched>, String> {
    let grouped = catcsv::grouped(body, &NAMESPACE, catalogue.structure, catalogue.entry)?;
    Ok(grouped
        .lists
        .into_iter()
        .map(|(raw, events)| list(catalogue, &raw, &grouped.columns, events))
        .collect())
}

/// A catalogue holding one list has the catalogue's name and key, else the list's own.
fn list(catalogue: &Catalogue, raw: &str, columns: &[String], events: Vec<EventRecord>) -> Fetched {
    let own = catalogue.name.to_lowercase();
    let (key, name) = match raw.is_empty() {
        true => (own, catalogue.name),
        false => (
            format!("{own}.{}", raw.to_lowercase()),
            catcsv::observer_name(raw),
        ),
    };
    Fetched {
        catalogue: Uuid::new_v5(&NAMESPACE, key.as_bytes()).to_string(),
        key,
        folder: format!("HELIO4CAST/{}", catalogue.name),
        name: name.to_owned(),
        description: (catalogue.describe)(name),
        url: catalogue.page.to_owned(),
        columns: columns.to_vec(),
        events: Ok(events),
    }
}

#[async_trait]
impl Provider for Helio4cast {
    fn id(&self) -> &'static str {
        "helio4cast"
    }

    fn source(&self) -> &'static str {
        "HELIO4CAST (Möstl et al., helioforecast.space)"
    }

    fn default_schedule(&self) -> &'static str {
        "45 6 * * 1"
    }

    async fn fetch(
        &self,
        http: &reqwest::Client,
        _settings: &Value,
        _refreshed: &HashMap<String, Timestamp>,
    ) -> Result<Vec<Fetched>, String> {
        // simplify: one failing catalogue fails the run on purpose. List keys are only known from
        // the data, so returning the healthy ones would let the engine unpublish the failed
        // catalogue's lists. Upgrade path: fixed keys for a catalogue that has them.
        let mut all = Vec::new();
        for catalogue in &CATALOGUES {
            all.extend(fetch_catalogue(http, catalogue).await?);
        }
        Ok(all)
    }
}

async fn fetch_catalogue(
    http: &reqwest::Client,
    catalogue: &Catalogue,
) -> Result<Vec<Fetched>, String> {
    let page = catcsv::download(http, catalogue.page).await?;
    let url = catcsv::csv_url(catalogue.page, &page, catalogue.tag)?;
    let body = catcsv::download(http, &url).await?;
    lists(catalogue, &body).map_err(|error| format!("{}: {error}", catalogue.name))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const ARRCAT: &str = include_str!("../../tests/fixtures/relay/arrcat_sample.csv");
    const SIRCAT: &str = include_str!("../../tests/fixtures/relay/sircat_sample.csv");
    const LINEUPS: &str = include_str!("../../tests/fixtures/relay/lineups_sample.csv");

    fn at(text: &str) -> Timestamp {
        Timestamp::parse(text).unwrap()
    }

    fn catalogue(name: &str) -> &'static Catalogue {
        CATALOGUES.iter().find(|c| c.name == name).unwrap()
    }

    fn all(name: &str, body: &str) -> Vec<Fetched> {
        lists(catalogue(name), body).unwrap()
    }

    fn list(name: &str, body: &str, key: &str) -> Fetched {
        all(name, body)
            .into_iter()
            .find(|list| list.key == key)
            .unwrap()
    }

    fn event(name: &str, body: &str, key: &str, id: &str) -> EventRecord {
        let uuid = Uuid::new_v5(&NAMESPACE, id.as_bytes()).to_string();
        list(name, body, key)
            .events
            .unwrap()
            .into_iter()
            .find(|event| event.uuid == uuid)
            .unwrap()
    }

    fn keys(name: &str, body: &str) -> Vec<String> {
        all(name, body).into_iter().map(|list| list.key).collect()
    }

    #[test]
    fn each_page_links_its_current_csv() {
        let pages = [
            (
                "ARRCAT",
                include_str!("../../tests/fixtures/relay/arrcat_page_links.html"),
                "arrcat/HELCATS_ARRCAT_v20.csv",
            ),
            (
                "SIRCAT",
                include_str!("../../tests/fixtures/relay/sircat_page_links.html"),
                "sircat/HELIO4CAST_SIRCAT_v10.csv",
            ),
            (
                "LineupCAT",
                include_str!("../../tests/fixtures/relay/lineups_page_links.html"),
                "lineups/HELIO4CAST_multipoint_v30.csv",
            ),
        ];

        for (name, page, file) in pages {
            let c = catalogue(name);
            assert_eq!(
                catcsv::csv_url(c.page, page, c.tag).unwrap(),
                format!("https://helioforecast.space/static/sync/{file}")
            );
        }
    }

    #[test]
    fn arrcat_has_one_catalogue_per_target_in_its_folder() {
        assert_eq!(
            keys("ARRCAT", ARRCAT),
            [
                "arrcat.bepicolombo",
                "arrcat.earth_l1",
                "arrcat.juno",
                "arrcat.mercury",
                "arrcat.stereo-a"
            ]
        );
        let juno = list("ARRCAT", ARRCAT, "arrcat.juno");
        assert_eq!(juno.folder, "HELIO4CAST/ARRCAT");
        assert_eq!(juno.name, "Juno");
        assert_eq!(juno.url, "https://helioforecast.space/arrcat");
        assert!(juno.description.contains("co-authorship"));
        assert!(juno.description.contains("Juno"));
        assert_eq!(juno.events.unwrap().len(), 2);
        assert_eq!(list("ARRCAT", ARRCAT, "arrcat.earth_l1").name, "Earth L1");
    }

    #[test]
    fn an_arrcat_event_is_a_point_at_the_arrival() {
        let event = event("ARRCAT", ARRCAT, "arrcat.juno", "HCME_A__20250226_01:JUNO");

        assert_eq!(event.start, at("2025-03-08T14:26:00Z"));
        assert_eq!(event.stop, event.start);
    }

    #[test]
    fn an_arrcat_cme_hitting_two_targets_is_two_events_with_their_own_keys() {
        let mercury = event(
            "ARRCAT",
            ARRCAT,
            "arrcat.mercury",
            "HCME_A__20250226_01:Mercury",
        );
        let juno = event("ARRCAT", ARRCAT, "arrcat.juno", "HCME_A__20250226_01:JUNO");

        assert_ne!(mercury.uuid, juno.uuid);
    }

    #[test]
    fn arrcat_parameters_keep_their_names_with_the_unit() {
        let a = event("ARRCAT", ARRCAT, "arrcat.juno", "HCME_A__20250226_01:JUNO").attributes;

        assert_eq!(a["sc"], json!("A"));
        assert_eq!(a["sse_launch_time"], json!("2025-02-26T15:52:00Z"));
        assert_eq!(a["target_arrival_time_err_h"], json!(28.8));
        assert_eq!(a["target_distance_au"], json!(5.098));
        assert_eq!(a["target_heeq_lon_deg"], json!(-84.4));
        assert_eq!(a["target_delta_deg"], json!(6.6));
        assert_eq!(a["sse_speed_km_s"], json!(900.0));
        assert_eq!(a["sse_speed_err_km_s"], json!(107.0));
        assert_eq!(a["target_speed_km_s"], json!(888.0));
        assert_eq!(a["pa_center_deg"], json!(107.5));
        for structural in ["id", "target_name", "target_arrival_time"] {
            assert!(!a.contains_key(structural), "{structural}");
        }
    }

    #[test]
    fn arrcat_columns_follow_the_files_order() {
        let columns = list("ARRCAT", ARRCAT, "arrcat.juno").columns;

        assert_eq!(columns[0], "sc");
        assert_eq!(columns[1], "sse_launch_time");
        assert_eq!(columns[2], "target_arrival_time_err_h");
        assert_eq!(columns.last().unwrap(), "pa_center_deg");
        assert_eq!(columns.len(), 17);
    }

    #[test]
    fn sircat_has_one_catalogue_per_spacecraft_in_its_folder() {
        assert_eq!(
            keys("SIRCAT", SIRCAT),
            ["sircat.maven", "sircat.stereo-a", "sircat.wind"]
        );
        let wind = list("SIRCAT", SIRCAT, "sircat.wind");
        assert_eq!(wind.folder, "HELIO4CAST/SIRCAT");
        assert_eq!(wind.name, "Wind");
        assert_eq!(wind.url, "https://helioforecast.space/sircat");
        assert!(wind.description.contains("co-authorship"));
        assert_eq!(wind.events.unwrap().len(), 2);
    }

    #[test]
    fn a_sir_runs_from_its_start_to_the_later_of_its_two_ends() {
        let maven = event(
            "SIRCAT",
            SIRCAT,
            "sircat.maven",
            "SIR_MAVEN_HUANG_20141203_01",
        );

        assert_eq!(maven.start, at("2014-12-03T03:25:00Z"));
        assert_eq!(maven.stop, at("2014-12-03T22:56:00Z"));
    }

    #[test]
    fn a_stream_without_sir_times_runs_from_its_high_speed_start_to_its_end() {
        let wind = event(
            "SIRCAT",
            SIRCAT,
            "sircat.wind",
            "SIR_WIND_GRANDIN_20070109_01",
        );

        assert_eq!(wind.start, at("2007-01-09T04:00:00Z"));
        assert_eq!(wind.stop, at("2007-01-12T05:00:00Z"));
    }

    #[test]
    fn sircat_parameters_keep_their_names_with_the_unit_and_skip_what_is_empty() {
        let a = event(
            "SIRCAT",
            SIRCAT,
            "sircat.wind",
            "SIR_WIND_GRANDIN_20070109_01",
        )
        .attributes;

        assert_eq!(a["sc_heliodistance_au"], json!(0.9726));
        assert_eq!(a["sc_long_heeq_deg"], json!(0.1));
        assert_eq!(a["hss_vtmax_km_s"], json!(566.7));
        assert_eq!(a["hss_btmax_nt"], json!(8.8));
        assert_eq!(a["hss_duration_h"], json!(73.0));
        assert_eq!(a["hss_vtmax_time"], json!("2007-01-11T17:34:00Z"));
        assert_eq!(a["hss_end_time"], json!("2007-01-12T05:00:00Z"));
        assert!(!a.contains_key("sir_end_time") && !a.contains_key("sir_vtmean_km_s"));
        assert!(!a.contains_key("sircat_id") && !a.contains_key("sc_insitu"));
        assert!(!a.contains_key("sir_start_time"));
    }

    #[test]
    fn lineupcat_is_one_catalogue_of_points_at_the_event_start() {
        let lists = all("LineupCAT", LINEUPS);

        assert_eq!(lists.len(), 1);
        let lineups = &lists[0];
        assert_eq!(lineups.key, "lineupcat");
        assert_eq!(lineups.folder, "HELIO4CAST/LineupCAT");
        assert_eq!(lineups.name, "LineupCAT");
        assert_eq!(lineups.url, "https://helioforecast.space/lineups");
        assert!(lineups.description.contains("co-authorship"));
        assert_eq!(lineups.events.as_ref().unwrap().len(), 8);
        let wind = event(
            "LineupCAT",
            LINEUPS,
            "lineupcat",
            "MULTI_2024-12-15_01:Wind:ICME_Wind_MOESTL_20241217_01",
        );
        assert_eq!(wind.start, at("2024-12-17T04:28:00Z"));
        assert_eq!(wind.stop, wind.start);
    }

    #[test]
    fn two_rows_of_a_lineup_on_one_spacecraft_stay_two_events() {
        let psp = |id| {
            event(
                "LineupCAT",
                LINEUPS,
                "lineupcat",
                &format!("MULTI_2024-08-11_01:PSP:{id}"),
            )
        };

        let late = psp("ICME_PSP_MOESTL_20240811_02");
        let early = psp("ICME_PSP_MOESTL_20240811_01");

        assert_ne!(late.uuid, early.uuid);
    }

    #[test]
    fn lineupcat_keeps_its_ids_and_its_parameters_with_the_unit() {
        let a = event(
            "LineupCAT",
            LINEUPS,
            "lineupcat",
            "MULTI_2024-12-15_01:Wind:ICME_Wind_MOESTL_20241217_01",
        )
        .attributes;

        assert_eq!(a["event"], json!(1.0));
        assert_eq!(a["lineupcat_id"], json!("MULTI_2024-12-15_01"));
        assert_eq!(a["spacecraft"], json!("Wind"));
        assert_eq!(a["catalog_id"], json!("ICME_Wind_MOESTL_20241217_01"));
        assert_eq!(a["sc_heliodistance_au"], json!(0.973));
        assert_eq!(a["sc_heeq_lon_deg"], json!(-0.1));
        assert_eq!(a["speed_km_s"], json!(622.0));
        assert_eq!(a["mo_bmax_nt"], json!(32.0));
        assert_eq!(a["mo_bzmin_nt"], json!(-18.2));
        assert!(!a.contains_key("event_start_time"));
    }

    #[test]
    fn a_dash_or_a_blank_is_no_attribute() {
        let wind = event(
            "LineupCAT",
            LINEUPS,
            "lineupcat",
            "MULTI_2024-12-15_01:Wind:ICME_Wind_MOESTL_20241217_01",
        )
        .attributes;
        let hia = event(
            "LineupCAT",
            LINEUPS,
            "lineupcat",
            "MULTI_2024-12-15_01:HIA:HCME_A__20241215_01",
        )
        .attributes;
        let soho = event(
            "LineupCAT",
            LINEUPS,
            "lineupcat",
            "MULTI_2024-07-21_01:SOHO: LASCO/C2:DONKI_2024-07-21T16:48:00-CME-001",
        )
        .attributes;

        assert!(!wind.contains_key("cme_heeq_lon_deg") && !wind.contains_key("source"));
        assert!(!hia.contains_key("mo_bmax_nt") && hia.contains_key("cme_heeq_lon_deg"));
        assert_eq!(soho["source"], json!("N15E20"));
    }

    #[test]
    fn an_empty_file_or_a_bad_time_is_an_error() {
        for (name, body) in [
            ("ARRCAT", ARRCAT),
            ("SIRCAT", SIRCAT),
            ("LineupCAT", LINEUPS),
        ] {
            let header = body.lines().next().unwrap();
            assert!(lists(catalogue(name), header).is_err(), "{name}");
        }
        let broken = ARRCAT.replace("2025-03-08T14:26Z", "yesterday");
        assert!(
            lists(catalogue("ARRCAT"), &broken)
                .unwrap_err()
                .contains("HCME_A__20250226_01")
        );
    }

    #[test]
    fn the_provider_is_registered_with_no_setting() {
        let provider = crate::relay::providers()
            .into_iter()
            .find(|provider| provider.id() == "helio4cast")
            .unwrap();

        assert_eq!(
            provider.source(),
            "HELIO4CAST (Möstl et al., helioforecast.space)"
        );
        assert!(provider.settings().is_empty());
        assert_eq!(provider.default_schedule(), "45 6 * * 1");
    }
}
