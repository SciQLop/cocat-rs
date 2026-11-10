//! AMDA (CDPP, IRAP): the catalogues and time tables its users share. Each shared list is the
//! public catalogue `public.amda.<AMDA id>`, filed under `AMDA/` and AMDA's folder.
//!
//! The ids, uuids and attribute values are the ones the curator script `tools/amda_public.py`
//! wrote before this provider replaced it, so the catalogues it made are adopted as they are.
//! The script read the same VOTables through speasy and wrote attributes with Python's rules,
//! which are kept here where they differ from cocat's own reading: text is trimmed, empty
//! values are left out and text that reads as a time is written as one.
//!
//! AMDA's REST service answers each request with the URL of a document it made: a tree of
//! the shared lists, or one list as a VOTable.

use std::borrow::Cow;
use std::collections::BTreeMap;

use async_trait::async_trait;
use cocat_core::model::EventRecord;
use cocat_core::time::Timestamp;
use cocat_core::votable;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use serde_json::{Map, Value};
use time::format_description::well_known::Iso8601;
use time::{OffsetDateTime, PrimitiveDateTime};
use uuid::Uuid;

use super::{Fetched, Provider};

pub struct Amda;

pub const SOURCE: &str = "AMDA";
const SITE: &str = "https://amda.irap.omp.eu/";
const REST: &str = "https://amda.irap.omp.eu/php/rest/";
/// Fixed forever: changing it changes every uuid and duplicates every event.
const NAMESPACE: Uuid = Uuid::from_u128(0x5b0f6a52_3c7e_4d61_9a0e_a3da00000001);
const MAX_DESCRIPTION: usize = 2000;
/// What a value starts with to be read as a time, as the script's `\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}`:
/// `d` a digit, `_` the date and time separator.
const TIME_SHAPE: &[u8] = b"dddd-dd-dd_dd:dd";

/// One of AMDA's two trees of shared lists, and how to ask for one of its lists.
struct Tree {
    list: &'static str,
    get: &'static str,
    id: &'static str,
}

const TREES: [Tree; 2] = [
    Tree {
        list: "getCatalogsList.php",
        get: "getCatalog.php",
        id: "catID",
    },
    Tree {
        list: "getTimeTablesList.php",
        get: "getTimeTable.php",
        id: "ttID",
    },
];

/// One shared list, as AMDA's tree describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct SharedList {
    pub id: String,
    pub name: String,
    /// `AMDA/` and the list's folders in the tree.
    pub folder: String,
    pub description: String,
}

/// Every shared list of one of AMDA's trees (catalogues, or time tables): an element with an
/// interval count, in the folders holding it, the root left out.
pub fn shared_lists(tree: &str) -> Result<Vec<SharedList>, String> {
    let mut reader = Reader::from_str(tree);
    let mut path: Vec<String> = Vec::new();
    let mut lists = Vec::new();
    loop {
        match reader.read_event().map_err(|error| error.to_string())? {
            Event::Start(element) => {
                if let Some(folders) = path.get(1..) {
                    lists.extend(shared_list(&element, folders)?);
                }
                path.push(attribute(&element, "name").unwrap_or_else(|| tag(&element)));
            }
            Event::Empty(element) => {
                if let Some(folders) = path.get(1..) {
                    lists.extend(shared_list(&element, folders)?);
                }
            }
            Event::End(_) => {
                path.pop();
            }
            Event::Eof => return Ok(lists),
            _ => {}
        }
    }
}

fn shared_list(element: &BytesStart<'_>, folders: &[String]) -> Result<Option<SharedList>, String> {
    if attribute(element, "nbIntervals").is_none() {
        return Ok(None);
    }
    let id = attribute(element, "xml:id").ok_or("a shared list has no id")?;
    let folder = std::iter::once(SOURCE)
        .chain(folders.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("/");
    Ok(Some(SharedList {
        id,
        name: attribute(element, "name").unwrap_or_else(|| tag(element)),
        folder,
        description: attribute(element, "description")
            .unwrap_or_default()
            .trim()
            .to_owned(),
    }))
}

fn attribute(element: &BytesStart<'_>, key: &str) -> Option<String> {
    element
        .attributes()
        .flatten()
        .find(|attribute| attribute.key.as_ref() == key)
        .and_then(|attribute| attribute.normalized_value(XmlVersion::Implicit1_0).ok())
        .map(Cow::into_owned)
}

fn tag(element: &BytesStart<'_>) -> String {
    element.name().as_ref().to_owned()
}

/// The list read from its VOTable, its first table as speasy reads it.
pub fn fetched_list(list: &SharedList, votable: &str) -> Result<Fetched, String> {
    let imported = votable::import(votable).map_err(|error| error.to_string())?;
    let table = imported
        .tables
        .into_iter()
        .next()
        .ok_or("the VOTable holds no table")?;
    Ok(Fetched {
        key: list.id.clone(),
        folder: list.folder.clone(),
        name: list.name.clone(),
        description: index_description(&list.description),
        url: SITE.to_owned(),
        catalogue: Uuid::new_v5(&NAMESPACE, list.id.as_bytes()).to_string(),
        events: events(&list.id, table.events)?,
    })
}

fn index_description(description: &str) -> String {
    if description.chars().count() <= MAX_DESCRIPTION {
        return description.to_owned();
    }
    description
        .chars()
        .take(MAX_DESCRIPTION - 1)
        .chain(['…'])
        .collect()
}

/// The rows as AMDA's events; the n-th copy of a (start, stop) pair gets occurrence n.
fn events(list: &str, rows: Vec<EventRecord>) -> Result<Vec<EventRecord>, String> {
    let mut seen: BTreeMap<(Timestamp, Timestamp), usize> = BTreeMap::new();
    rows.into_iter()
        .map(|row| {
            let occurrence = seen.entry((row.start, row.stop)).or_default();
            let mut event = EventRecord::new(
                event_uuid(list, row.start, row.stop, *occurrence)?,
                row.start,
                row.stop,
            );
            *occurrence += 1;
            event.author = Some(SOURCE.to_owned());
            event.attributes = attributes(row.attributes);
            Ok(event)
        })
        .collect()
}

fn event_uuid(
    list: &str,
    start: Timestamp,
    stop: Timestamp,
    occurrence: usize,
) -> Result<String, String> {
    let name = format!(
        "{list}/{}/{}/{occurrence}",
        isoformat(start)?,
        isoformat(stop)?
    );
    Ok(Uuid::new_v5(&NAMESPACE, name.as_bytes()).to_string())
}

/// Python's `datetime.isoformat()` of the aware UTC instant, the text the script hashed.
fn isoformat(at: Timestamp) -> Result<String, String> {
    let text = at.as_text().map_err(|error| error.to_string())?;
    Ok(text.replacen(' ', "T", 1))
}

/// The columns as the script wrote them: empty values left out, text trimmed, and text that
/// reads as a time written as one.
fn attributes(columns: Map<String, Value>) -> Map<String, Value> {
    columns
        .into_iter()
        .filter_map(|(key, value)| Some((key, value_of(value)?)))
        .collect()
}

fn value_of(value: Value) -> Option<Value> {
    match value {
        Value::Null => None,
        Value::String(text) => text_of(text.trim()),
        Value::Array(items) => Some(Value::Array(
            items
                .into_iter()
                .map(|item| value_of(item).unwrap_or(Value::Null))
                .collect(),
        )),
        other => Some(other),
    }
}

fn text_of(text: &str) -> Option<Value> {
    if text.is_empty() {
        return None;
    }
    let time = looks_like_a_time(text).then(|| python_time(text)).flatten();
    Some(Value::String(
        time.map_or_else(|| text.to_owned(), |time| time.to_string()),
    ))
}

fn looks_like_a_time(text: &str) -> bool {
    text.len() >= TIME_SHAPE.len()
        && text
            .bytes()
            .zip(TIME_SHAPE)
            .all(|(byte, shape)| match shape {
                b'd' => byte.is_ascii_digit(),
                b'_' => byte == b'T' || byte == b' ',
                _ => byte == *shape,
            })
}

/// The instant `datetime.fromisoformat` reads in `text`, naive meaning UTC; `text` has the
/// time shape, so its date ends at byte 10.
fn python_time(text: &str) -> Option<Timestamp> {
    let iso = format!("{}T{}", &text[..10], &text[11..]);
    let instant = OffsetDateTime::parse(&iso, &Iso8601::DEFAULT)
        .or_else(|_| PrimitiveDateTime::parse(&iso, &Iso8601::DEFAULT).map(|at| at.assume_utc()))
        .ok()?;
    Timestamp::from_unix_nanos(i64::try_from(instant.unix_timestamp_nanos()).ok()?)
}

/// AMDA answers a request with the URL of the document it made; this reads the document.
/// Only a URL on AMDA's own site is followed.
async fn document(
    http: &reqwest::Client,
    request: reqwest::RequestBuilder,
) -> Result<String, String> {
    let answer = text(request).await?;
    let url = answer.trim();
    if !url.starts_with(SITE) {
        return Err(format!("AMDA answered {url:.200}"));
    }
    text(http.get(url)).await
}

async fn text(request: reqwest::RequestBuilder) -> Result<String, String> {
    let describe = |error: reqwest::Error| error.without_url().to_string();
    request
        .send()
        .await
        .map_err(describe)?
        .error_for_status()
        .map_err(describe)?
        .text()
        .await
        .map_err(describe)
}

async fn fetch_tree(http: &reqwest::Client, tree: &Tree) -> Result<Vec<Fetched>, String> {
    let lists = document(http, http.get(format!("{REST}{}", tree.list)))
        .await
        .and_then(|xml| shared_lists(&xml))
        .map_err(|error| format!("{}: {error}", tree.list))?;
    let mut fetched = Vec::with_capacity(lists.len());
    for list in lists {
        let request = http
            .get(format!("{REST}{}", tree.get))
            .query(&[(tree.id, &list.id)]);
        let votable = document(http, request).await;
        let read = votable.and_then(|votable| fetched_list(&list, &votable));
        fetched.push(read.map_err(|error| format!("{}: {error}", list.id))?);
    }
    Ok(fetched)
}

#[async_trait]
impl Provider for Amda {
    fn id(&self) -> &'static str {
        "amda"
    }

    fn source(&self) -> &'static str {
        SOURCE
    }

    fn default_schedule(&self) -> &'static str {
        "0 4 * * *"
    }

    /// One list AMDA cannot give fails the run, so no catalogue changes and the
    /// administrator reads which list it was.
    async fn fetch(
        &self,
        http: &reqwest::Client,
        _settings: &Value,
        _last_ok: Option<Timestamp>,
    ) -> Result<Vec<Fetched>, String> {
        let mut fetched = Vec::new();
        for tree in &TREES {
            fetched.extend(fetch_tree(http, tree).await?);
        }
        Ok(fetched)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const CATALOGS: &str = include_str!("../../tests/fixtures/relay/amda_catalogs.xml");
    const TIME_TABLES: &str = include_str!("../../tests/fixtures/relay/amda_timetables.xml");
    const SOLO: &str = include_str!("../../tests/fixtures/relay/amda_sharedcatalog_63.xml");
    const SUBSTORMS: &str = include_str!("../../tests/fixtures/relay/amda_sharedtimeTable_10.xml");

    /// An AMDA-like table, every column text: one interval three times, one at a quarter
    /// second, blank and padded labels, and times written four ways.
    const COPIES: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<VOTABLE version="1.5" xmlns="http://www.ivoa.net/xml/VOTable/v1.3"><DESCRIPTION>Name:chars;
</DESCRIPTION><RESOURCE><TABLE><FIELD name="Start Time" datatype="char" arraysize="*"/><FIELD name="Stop Time" datatype="char" arraysize="*"/><FIELD name="label" datatype="char" arraysize="*"/><FIELD name="seen" datatype="char" arraysize="*"/><DATA><TABLEDATA><TR><TD>2020-01-01T00:00:00.000</TD><TD>2020-01-01T01:00:00.000</TD><TD> MSH </TD><TD>2019-01-01T00:00:04.500</TD></TR><TR><TD>2020-01-01T00:00:00.000</TD><TD>2020-01-01T01:00:00.000</TD><TD></TD><TD>2019-01-01 00:00</TD></TR><TR><TD>2020-01-01T00:00:00.250</TD><TD>2020-01-01T01:00:00.000</TD><TD>  </TD><TD>2019-01-01T00:00:04 UT</TD></TR><TR><TD>2020-01-01T00:00:00.000</TD><TD>2020-01-01T01:00:00.000</TD><TD>x</TD><TD>2019-01-01T00:00:04+01:00</TD></TR></TABLEDATA></DATA></TABLE></RESOURCE></VOTABLE>"#;

    fn list(tree: &str, id: &str) -> SharedList {
        shared_lists(tree)
            .unwrap()
            .into_iter()
            .find(|list| list.id == id)
            .unwrap()
    }

    fn uuids(fetched: &Fetched) -> Vec<&str> {
        fetched.events.iter().map(|e| e.uuid.as_str()).collect()
    }

    #[test]
    fn the_tree_gives_each_list_its_folder() {
        let placed = |tree| {
            shared_lists(tree)
                .unwrap()
                .into_iter()
                .map(|list| (list.id, list.folder, list.name))
                .collect::<Vec<_>>()
        };
        let at = |id: &str, folder: &str, name: &str| (id.into(), folder.into(), name.into());

        assert_eq!(
            placed(CATALOGS),
            [
                at(
                    "sharedcatalog_21",
                    "AMDA/BepiColombo_Cruise_Science_WG",
                    "BepiCoordObs_Windows_of_Opportunities"
                ),
                at("sharedcatalog_63", "AMDA/SPACECRAFT_ORBIT_NUMBERS", "solo"),
            ]
        );
        assert_eq!(
            placed(TIME_TABLES),
            [
                at("sharedtimeTable_0", "AMDA/EARTH", "FTE_c1"),
                at(
                    "sharedtimeTable_10",
                    "AMDA/EARTH",
                    "THEMIS_Substorm_2007-2008_CHBG"
                ),
                at("sharedtimeTable_51", "AMDA/JUPITER", "callisto_flyby"),
            ]
        );
        let loose = r#"<timeTableList xml:id="root"><folder name="A"><folder name="B C"><timeTable xml:id="t1" name="deep" nbIntervals="1"/></folder></folder><timeTable xml:id="t2" name="loose" nbIntervals="1"/></timeTableList>"#;
        assert_eq!(
            placed(loose),
            [at("t1", "AMDA/A/B C", "deep"), at("t2", "AMDA", "loose")]
        );
    }

    #[test]
    fn a_description_is_read_as_the_script_read_it() {
        let substorms = list(TIME_TABLES, "sharedtimeTable_10");

        assert!(
            substorms
                .description
                .starts_with("THEMIS substorms list based on overview jpegs"),
            "{}",
            substorms.description
        );
        assert!(substorms.description.ends_with(
            "for more details; \n        Historic: From old AMDA;\n        Creation Date :  2013-11-22T13:52:49;"
        ));
    }

    #[test]
    fn a_long_description_is_cut_for_the_index() {
        let bepi = list(CATALOGS, "sharedcatalog_21");

        let fetched = fetched_list(&bepi, SOLO).unwrap();

        assert_eq!(bepi.description.chars().count(), 2206);
        assert_eq!(fetched.description.chars().count(), 2000);
        assert!(fetched.description.ends_with("hee min_m…"));
        let solo = list(CATALOGS, "sharedcatalog_63");
        assert_eq!(
            fetched_list(&solo, SOLO).unwrap().description,
            solo.description
        );
    }

    /// The uuids below are the script's, computed by `tools/amda_public.py` with speasy
    /// reading the same files.
    #[test]
    fn a_list_keeps_the_scripts_ids() {
        let solo = fetched_list(&list(CATALOGS, "sharedcatalog_63"), SOLO).unwrap();
        let substorms = fetched_list(&list(TIME_TABLES, "sharedtimeTable_10"), SUBSTORMS).unwrap();

        assert_eq!(solo.key, "sharedcatalog_63");
        assert_eq!(solo.catalogue, "931dccc5-a77f-5e2b-8702-53b2df03b6ba");
        assert_eq!(
            uuids(&solo),
            [
                "b54195cf-d8f9-5d48-85fa-11ffa44fae65",
                "46435578-93af-5fc1-8417-87391644c560",
                "63e3d16b-22c2-5f8f-8683-56688daaa73f",
            ]
        );
        assert_eq!(
            serde_json::Value::Object(solo.events[0].attributes.clone()),
            json!({"Orbit No.": "1", "Semi Axis.": "112546934.18", "Incl.": "22.07",
                   "Eccen.": "0.314", "Lon Asc.": "3.06", "Arg. Peri.": "323.55"})
        );
        assert_eq!(solo.events[0].author.as_deref(), Some(SOURCE));
        assert_eq!(substorms.key, "sharedtimeTable_10");
        assert_eq!(substorms.catalogue, "9053fd48-77cc-5d57-ace5-8f05d2b5648d");
        assert_eq!(
            uuids(&substorms),
            [
                "e04ff100-5dba-5528-86fc-536137c24f96",
                "29557756-170e-5726-9b78-8dcea7cd9c0f",
                "64c3e598-5f93-5b58-980d-a3edc4332173",
                "600ebe10-677c-5c6f-ba34-03a2bdf6737c",
            ]
        );
        assert!(substorms.events.iter().all(|e| e.attributes.is_empty()));
    }

    #[test]
    fn the_nth_copy_of_an_interval_gets_occurrence_n() {
        let copies = SharedList {
            id: "sharedcatalog_1".into(),
            name: "copies".into(),
            folder: "AMDA".into(),
            description: String::new(),
        };

        let fetched = fetched_list(&copies, COPIES).unwrap();

        assert_eq!(
            uuids(&fetched),
            [
                "27374bde-8926-54b6-9c87-bc180e106936",
                "b2a1c3bd-8dde-5e1b-9dc8-6c3964e2d50b",
                "367dd0a4-dbbe-59ea-a019-c4969a7f3a00",
                "75f61768-139f-51dc-ba01-a1100eecabfe",
            ]
        );
    }

    #[test]
    fn columns_become_attributes_as_the_script_wrote_them() {
        let copies = SharedList {
            id: "sharedcatalog_1".into(),
            name: "copies".into(),
            folder: "AMDA".into(),
            description: String::new(),
        };

        let fetched = fetched_list(&copies, COPIES).unwrap();

        let attributes: Vec<Value> = fetched
            .events
            .iter()
            .map(|e| Value::Object(e.attributes.clone()))
            .collect();
        assert_eq!(
            attributes,
            [
                json!({"label": "MSH", "seen": "2019-01-01T00:00:04.500000Z"}),
                json!({"seen": "2019-01-01T00:00:00.000000Z"}),
                json!({"seen": "2019-01-01T00:00:04 UT"}),
                json!({"label": "x", "seen": "2018-12-31T23:00:04.000000Z"}),
            ]
        );
    }

    /// What Python 3.14's `datetime.fromisoformat` made of each text, then cocat's time text.
    #[test]
    fn text_python_reads_as_a_time_becomes_one() {
        let read = [
            ("2019-01-01T00:00", "2019-01-01T00:00:00.000000Z"),
            ("2019-01-01 00:00:04", "2019-01-01T00:00:04.000000Z"),
            ("2019-01-01T00:00:04,5", "2019-01-01T00:00:04.500000Z"),
            ("2019-01-01T00:00:04.1234567", "2019-01-01T00:00:04.123456Z"),
            ("2019-01-01T00:00:04Z", "2019-01-01T00:00:04.000000Z"),
            ("2019-01-01T00:00:04+0100", "2018-12-31T23:00:04.000000Z"),
            ("2019-01-01T00:00:04+01", "2018-12-31T23:00:04.000000Z"),
            ("2019-01-01T00:00:60", "2019-01-01T00:00:60"),
            ("2019-01-01", "2019-01-01"),
            ("1 2019-01-01T00:00", "1 2019-01-01T00:00"),
        ];

        for (text, written) in read {
            assert_eq!(text_of(text), Some(Value::from(written)), "{text}");
        }
    }

    #[test]
    fn a_tree_that_is_not_xml_is_an_error() {
        assert!(shared_lists("<catalogList><folder></catalogList>").is_err());
        assert!(fetched_list(&list(CATALOGS, "sharedcatalog_63"), "no table").is_err());
    }

    #[test]
    fn the_provider_is_registered_daily_with_no_setting() {
        let provider = crate::relay::providers()
            .into_iter()
            .find(|provider| provider.id() == "amda")
            .unwrap();

        assert_eq!(provider.source(), "AMDA");
        assert_eq!(provider.default_schedule(), "0 4 * * *");
        assert!(provider.settings().is_empty());
    }
}
