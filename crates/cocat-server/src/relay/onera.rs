//! ONERA's space weather event lists (Nguyen, Bernoux et al.), deposited on Recherche Data
//! Gouv under licence Etalab 2.0: the ICMEs, sheaths and SIRs seen in OMNI from 1995, the
//! radiation belt electron enhancements found in the Ca index from 1868, and the detections
//! of the paper that built the first ones (two neural networks and a threshold method).
//!
//! A run asks the repository for each dataset's latest file list, so a new version of a file
//! (`RB-events_v03-00_…`) is read without a change here, then downloads each file in the
//! format it was deposited in: the repository's tabular copy of the `;`-separated files
//! is one column wide. The download redirects to the repository's S3 storage.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use cocat_core::model::EventRecord;
use cocat_core::time::Timestamp;
use serde::Deserialize;
use serde_json::{Map, Value};
use uuid::Uuid;

use super::catcsv::{self, Row};
use super::{Fetched, Provider};

pub struct Onera;

const NAMESPACE: Uuid = Uuid::from_u128(0x3f1b8c27_6a4d_4e95_b0c2_8d7e1a5f9c63);
const API: &str = "https://entrepot.recherche.data.gouv.fr/api";
const BUSY_PAUSE: Duration = Duration::from_secs(30);

struct Dataset {
    doi: &'static str,
    paper: &'static str,
}

const CATALOGS: Dataset = Dataset {
    doi: "10.57745/BYC2WC",
    paper: "Nguyen, Bernoux & Ferlin (2025), Simultaneous multi-class detection of interplanetary space weather events, J. Space Weather Space Clim., https://doi.org/10.1051/swsc/2025016",
};
const ELECTRONS: Dataset = Dataset {
    doi: "10.57745/W4HY2S",
    paper: "Bernoux, Nguyen, Sicard & Maget (2025), Automatic identification of past radiation belts electron enhancement events using a ground-based parameter, Earth and Space Science, https://doi.org/10.1029/2024EA003945",
};
const RESULTS: Dataset = Dataset {
    doi: "10.57745/HDCAZ3",
    paper: CATALOGS.paper,
};
const DATASETS: [&Dataset; 3] = [&CATALOGS, &ELECTRONS, &RESULTS];

/// How a file is written: its separator, the columns holding the event's key and ends, how
/// it writes a time, and the columns kept as attributes, by attribute name. An attribute
/// named `…_time` is a time.
struct Layout {
    delimiter: u8,
    id: &'static str,
    start: &'static str,
    stop: &'static str,
    time: fn(&str) -> Option<Timestamp>,
    attributes: &'static [(&'static str, &'static str)],
}

/// `;`-separated, days first, the list each event was taken from in `origin`.
const OMNI: Layout = Layout {
    delimiter: b';',
    id: "",
    start: "begin",
    stop: "end",
    time: day_first,
    attributes: &[("origin", "origin")],
};
const ELECTRON_EVENTS: Layout = Layout {
    delimiter: b',',
    id: "id",
    start: "start_date",
    stop: "end_date",
    time: Timestamp::parse,
    attributes: &[
        ("max_date", "max_time"),
        ("max_value", "max_ca_nt"),
        ("category", "category"),
    ],
};
/// The detections write the network's confidence under `origin`; the threshold method has none.
const DETECTIONS: Layout = Layout {
    delimiter: b',',
    id: "",
    start: "begin",
    stop: "end",
    time: Timestamp::parse,
    attributes: &[("origin", "score")],
};

struct List {
    key: &'static str,
    folder: &'static str,
    name: &'static str,
    dataset: &'static Dataset,
    /// The file's name up to its version or extension.
    file: &'static str,
    layout: &'static Layout,
    about: &'static str,
}

const LISTS: [List; 10] = [
    List {
        key: "icme",
        folder: "ONERA",
        name: "ICMEs (OMNI)",
        dataset: &CATALOGS,
        file: "ICME_catalog_OMNI",
        layout: &OMNI,
        about: "Interplanetary coronal mass ejections seen at L1 in OMNI from 1995, gathered from existing catalogues and new finds; `origin` names the list each comes from.",
    },
    List {
        key: "sheath",
        folder: "ONERA",
        name: "Sheaths (OMNI)",
        dataset: &CATALOGS,
        file: "Sheath_catalog_OMNI",
        layout: &OMNI,
        about: "ICME sheaths seen at L1 in OMNI from 1995; `origin` names the list each comes from.",
    },
    List {
        key: "sir",
        folder: "ONERA",
        name: "SIRs (OMNI)",
        dataset: &CATALOGS,
        file: "SIR_catalog_OMNI",
        layout: &OMNI,
        about: "Stream interaction regions seen at L1 in OMNI from 1995; `origin` names the list each comes from.",
    },
    List {
        key: "electrons",
        folder: "ONERA",
        name: "Radiation belt electron enhancements (from 1868)",
        dataset: &ELECTRONS,
        file: "RB-events_",
        layout: &ELECTRON_EVENTS,
        about: "Enhancements of medium-energy (0.03-1 MeV) electrons at the core of the outer belt, found in the ground-based Ca index from 1868; `max_ca_nt` is the event's largest Ca, `category` its probable cause.",
    },
    List {
        key: "icme-resunet",
        folder: "ONERA/Detections",
        name: "ICMEs, ResUnet++",
        dataset: &RESULTS,
        file: "ICMEs_resunet",
        layout: &DETECTIONS,
        about: "ICMEs the ResUnet++ network detects in OMNI from 1995; `score` is its confidence.",
    },
    List {
        key: "icme-spodify",
        folder: "ONERA/Detections",
        name: "ICMEs, SPODIfY",
        dataset: &RESULTS,
        file: "ICMEs_spodify",
        layout: &DETECTIONS,
        about: "ICMEs the SPODIfY network detects in OMNI from 1995; `score` is its confidence.",
    },
    List {
        key: "sir-resunet",
        folder: "ONERA/Detections",
        name: "SIRs, ResUnet++",
        dataset: &RESULTS,
        file: "SIRs_resunet",
        layout: &DETECTIONS,
        about: "SIRs the ResUnet++ network detects in OMNI from 1995; `score` is its confidence.",
    },
    List {
        key: "sir-spodify",
        folder: "ONERA/Detections",
        name: "SIRs, SPODIfY",
        dataset: &RESULTS,
        file: "SIRs_spodify",
        layout: &DETECTIONS,
        about: "SIRs the SPODIfY network detects in OMNI from 1995; `score` is its confidence.",
    },
    List {
        key: "icme-thresholds",
        folder: "ONERA/Detections",
        name: "ICMEs, threshold method",
        dataset: &RESULTS,
        file: "thresholds_ICMEs",
        layout: &DETECTIONS,
        about: "ICMEs found in OMNI from 1995 by a threshold method after Lepping et al. (2005), the paper's baseline.",
    },
    List {
        key: "sir-thresholds",
        folder: "ONERA/Detections",
        name: "SIRs, threshold method",
        dataset: &RESULTS,
        file: "thresholds_SIRs",
        layout: &DETECTIONS,
        about: "SIRs found in OMNI from 1995 by the threshold method of Grandin et al. (2019), the paper's baseline.",
    },
];

#[derive(Deserialize)]
struct Answer {
    data: Data,
}

#[derive(Deserialize)]
struct Data {
    #[serde(rename = "latestVersion")]
    latest_version: Version,
}

#[derive(Deserialize)]
struct Version {
    files: Vec<Deposited>,
}

#[derive(Deserialize)]
struct Deposited {
    #[serde(rename = "dataFile")]
    data_file: DataFile,
}

#[derive(Deserialize)]
struct DataFile {
    id: u64,
    filename: String,
}

/// The files of a dataset's latest version, from the repository's answer.
fn files(answer: &str) -> Result<Vec<DataFile>, String> {
    let answer: Answer = serde_json::from_str(answer)
        .map_err(|error| format!("the repository's file list: {error}"))?;
    let files = answer.data.latest_version.files.into_iter();
    Ok(files.map(|file| file.data_file).collect())
}

/// Where the list's file is downloaded, in the format it was deposited in.
fn file_url(files: &[DataFile], list: &List) -> Result<String, String> {
    files
        .iter()
        .find(|file| file.filename.starts_with(list.file))
        .map(|file| format!("{API}/access/datafile/{}?format=original", file.id))
        .ok_or_else(|| format!("no file {}… in the dataset, has it changed?", list.file))
}

/// `08/02/1995 03:40`, as the OMNI catalogues write a time.
fn day_first(text: &str) -> Option<Timestamp> {
    let (date, time) = text.trim().split_once(' ')?;
    let mut parts = date.split('/');
    let (day, month, year) = (parts.next()?, parts.next()?, parts.next()?);
    Timestamp::parse(&format!("{year}-{month}-{day}T{time}:00Z"))
}

fn events(list: &List, body: &str) -> Result<Vec<EventRecord>, String> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(list.layout.delimiter)
        .from_reader(body.as_bytes());
    let headers = reader.headers().map_err(|error| error.to_string())?.clone();
    let events = reader
        .records()
        .map(|record| {
            let row: Row = record
                .and_then(|record| record.deserialize(Some(&headers)))
                .map_err(|error| error.to_string())?;
            event(list, &row)
        })
        .collect::<Result<Vec<_>, _>>()?;
    match events.is_empty() {
        true => Err("no event found, has the file layout changed?".to_owned()),
        false => Ok(events),
    }
}

fn event(list: &List, row: &Row) -> Result<EventRecord, String> {
    let layout = list.layout;
    let id = catcsv::field(row, layout.id)?;
    let time = |column| catcsv::named(id, time(layout, column, catcsv::field(row, column)?));
    let uuid = Uuid::new_v5(&NAMESPACE, format!("{}/{id}", list.key).as_bytes());
    let mut event = EventRecord::new(uuid.to_string(), time(layout.start)?, time(layout.stop)?);
    event.attributes = catcsv::named(id, attributes(layout, row))?;
    Ok(event)
}

fn time(layout: &Layout, name: &str, text: &str) -> Result<Timestamp, String> {
    (layout.time)(text).ok_or_else(|| format!("bad {name}"))
}

/// The layout's attributes the row fills; a column the file lacks gives none.
fn attributes(layout: &Layout, row: &Row) -> Result<Map<String, Value>, String> {
    let mut attributes = Map::new();
    for (column, name) in layout.attributes {
        let Some(text) = row.get(*column).filter(|text| !catcsv::missing(text)) else {
            continue;
        };
        let value = match name.ends_with("_time") {
            true => Some(Value::String(
                time(layout, name, text)?
                    .rfc3339()
                    .map_err(|_| format!("bad {name}"))?,
            )),
            false => catcsv::number_or_text(text.trim()),
        };
        if let Some(value) = value {
            attributes.insert((*name).to_owned(), value);
        }
    }
    Ok(attributes)
}

fn fetched(list: &List, events: Result<Vec<EventRecord>, String>) -> Fetched {
    let dataset = list.dataset;
    Fetched {
        key: list.key.to_owned(),
        folder: list.folder.to_owned(),
        name: list.name.to_owned(),
        description: format!(
            "{} From ONERA, https://doi.org/{} (licence Etalab 2.0); see {}.",
            list.about, dataset.doi, dataset.paper
        ),
        url: format!("https://doi.org/{}", dataset.doi),
        catalogue: Uuid::new_v5(&NAMESPACE, list.key.as_bytes()).to_string(),
        columns: Vec::new(),
        events,
    }
}

async fn download(http: &reqwest::Client, url: &str) -> Result<String, String> {
    let bytes = super::patient::fetch(http, url, BUSY_PAUSE).await?;
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

async fn dataset_files(http: &reqwest::Client, dataset: &Dataset) -> Result<Vec<DataFile>, String> {
    let url = format!(
        "{API}/datasets/:persistentId/?persistentId=doi:{}",
        dataset.doi
    );
    files(&download(http, &url).await?)
}

async fn list_events(
    http: &reqwest::Client,
    files: &Result<Vec<DataFile>, String>,
    list: &List,
) -> Result<Vec<EventRecord>, String> {
    let url = file_url(files.as_ref().map_err(Clone::clone)?, list)?;
    events(list, &download(http, &url).await?)
}

#[async_trait]
impl Provider for Onera {
    fn id(&self) -> &'static str {
        "onera"
    }

    fn source(&self) -> &'static str {
        "ONERA"
    }

    fn default_schedule(&self) -> &'static str {
        "15 7 * * 1"
    }

    async fn fetch(
        &self,
        http: &reqwest::Client,
        _settings: &Value,
        _refreshed: &HashMap<String, Timestamp>,
    ) -> Result<Vec<Fetched>, String> {
        let mut files = HashMap::new();
        for dataset in DATASETS {
            files.insert(dataset.doi, dataset_files(http, dataset).await);
        }
        let mut lists = Vec::new();
        for list in &LISTS {
            let events = list_events(http, &files[list.dataset.doi], list).await;
            lists.push(fetched(list, events));
        }
        Ok(lists)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const ICME: &str = include_str!("../../tests/fixtures/relay/onera_icme_sample.csv");
    const SIR: &str = include_str!("../../tests/fixtures/relay/onera_sir_sample.csv");
    const ELECTRON: &str = include_str!("../../tests/fixtures/relay/onera_electrons_sample.csv");
    const DETECTED: &str = include_str!("../../tests/fixtures/relay/onera_detections_sample.csv");
    const THRESHOLD: &str = include_str!("../../tests/fixtures/relay/onera_thresholds_sample.csv");
    const RESULTS_FILES: &str =
        include_str!("../../tests/fixtures/relay/onera_results_dataset.json");

    fn list(key: &str) -> &'static List {
        LISTS.iter().find(|list| list.key == key).unwrap()
    }

    fn at(text: &str) -> Timestamp {
        Timestamp::parse(text).unwrap()
    }

    fn uuid(key: &str, id: &str) -> String {
        Uuid::new_v5(&NAMESPACE, format!("{key}/{id}").as_bytes()).to_string()
    }

    #[test]
    fn an_omni_time_is_read_day_first() {
        let events = events(list("icme"), ICME).unwrap();

        assert_eq!(events.len(), 5);
        assert_eq!(events[0].uuid, uuid("icme", "0"));
        assert_eq!(events[0].start, at("1995-02-08T03:40:00Z"));
        assert_eq!(events[0].stop, at("1995-02-10T14:45:00Z"));
        assert_eq!(events[4].attributes["origin"], json!("Additional"));
    }

    #[test]
    fn an_empty_origin_is_no_attribute() {
        let events = events(list("sir"), SIR).unwrap();

        assert!(events[0].attributes.is_empty());
        assert_eq!(events[1].attributes["origin"], json!("Additional"));
    }

    #[test]
    fn an_electron_event_keeps_its_peak_and_probable_cause() {
        let events = events(list("electrons"), ELECTRON).unwrap();

        let first = &events[0];
        assert_eq!(first.start, at("1868-02-06T18:00:00Z"));
        assert_eq!(first.stop, at("1868-02-08T00:00:00Z"));
        assert_eq!(first.attributes["max_time"], json!("1868-02-06T21:00:00Z"));
        assert_eq!(first.attributes["max_ca_nt"], json!(19.3));
        assert!(!first.attributes.contains_key("category"));
        let last = events.last().unwrap();
        assert_eq!(last.uuid, uuid("electrons", "4337"));
        assert_eq!(last.attributes["category"], json!("multiple_icmes"));
    }

    #[test]
    fn a_detection_keeps_its_score_to_the_nanosecond() {
        let events = events(list("icme-spodify"), DETECTED).unwrap();

        assert_eq!(
            events[0].start.unix_micros(),
            at("1995-01-02T06:06:37.323089Z").unix_micros()
        );
        assert_eq!(events[0].attributes["score"], json!(0.45947757));
    }

    #[test]
    fn a_threshold_detection_has_no_score() {
        let events = events(list("icme-thresholds"), THRESHOLD).unwrap();

        assert_eq!(events[1].start, at("1995-02-08T10:40:00Z"));
        assert!(events[1].attributes.is_empty());
    }

    #[test]
    fn a_bad_time_names_the_event() {
        let broken = ICME.replace("04/03/1995 12:00", "yesterday");

        assert!(
            events(list("icme"), &broken)
                .unwrap_err()
                .contains("1: bad begin")
        );
    }

    #[test]
    fn an_empty_file_is_an_error() {
        let header = ICME.lines().next().unwrap();

        assert!(events(list("icme"), header).is_err());
    }

    #[test]
    fn each_result_list_finds_its_original_file() {
        let files = files(RESULTS_FILES).unwrap();
        let url = |key| file_url(&files, list(key)).unwrap();

        assert_eq!(
            url("icme-resunet"),
            format!("{API}/access/datafile/601083?format=original")
        );
        assert_eq!(
            url("sir-spodify"),
            format!("{API}/access/datafile/601086?format=original")
        );
        assert_eq!(
            url("sir-thresholds"),
            format!("{API}/access/datafile/601576?format=original")
        );
        assert!(
            file_url(&files, list("icme"))
                .unwrap_err()
                .contains("ICME_catalog_OMNI")
        );
    }

    #[test]
    fn a_list_reads_where_it_comes_from() {
        let list = fetched(list("electrons"), Ok(Vec::new()));

        assert_eq!(list.folder, "ONERA");
        assert_eq!(list.url, "https://doi.org/10.57745/W4HY2S");
        assert!(list.description.contains("10.1029/2024EA003945"));
        assert!(list.description.contains("Etalab"));
    }

    #[test]
    fn the_provider_is_registered_with_no_setting() {
        let provider = crate::relay::providers()
            .into_iter()
            .find(|provider| provider.id() == "onera")
            .unwrap();

        assert_eq!(provider.source(), "ONERA");
        assert!(provider.settings().is_empty());
    }
}
