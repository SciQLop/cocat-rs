//! The magnetopause crossings a random forest classifier found in THEMIS data, 2007 to mid-2022
//! (Grimmich et al. 2023), and in Cluster data, 2001-2020 (Grimmich et al. 2024), from their
//! datasets on OSF (CC-BY 4.0). One catalogue per mission, as the files are: a spacecraft is
//! an attribute. Klingenstein et al. (2025) use both.
//!
//! A provider of its own, not a literature list: about 220,000 crossings would add to the
//! peak memory of the literature run, which holds every list at once.
//!
//! Each file: `#` comment lines, a tab-separated header, then one crossing per line, its time
//! `2007-03-23 23:39:24` (UT). The second Cluster file adds the four-spacecraft analysis and
//! the solar wind to some of the first file's crossings.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use cocat_core::model::EventRecord;
use cocat_core::time::Timestamp;
use serde_json::{Map, Value};
use uuid::Uuid;

use super::{Fetched, Provider};

pub struct Grimmich;

const NAMESPACE: Uuid = Uuid::from_u128(0x91d48ef2_c1ff_48ce_8d99_b0085cb8f199);
const BUSY_PAUSE: Duration = Duration::from_secs(30);
const FOLDER: &str = "Literature/Earth";
const KLINGENSTEIN: &str = "Klingenstein et al. (2025) fit the subsolar stand-off distance of the magnetopause to the THEMIS and Cluster lists together: Ann. Geophys. 43, 835-854, https://doi.org/10.5194/angeo-43-835-2025.";

/// One mission's list.
struct List {
    key: &'static str,
    name: &'static str,
    about: &'static str,
    credit: &'static str,
    url: &'static str,
    /// The crossings, then for Cluster the file that adds an analysis to some of them. These
    /// are the OSF file server's addresses: the project pages' download links (osf.io/download)
    /// took 20 to 100 s to redirect there, close to the relay's 2 minute limit per request.
    files: &'static [&'static str],
    /// The attributes in order, the catalogue's columns.
    columns: &'static [&'static str],
    read: fn(&[String]) -> Result<Vec<EventRecord>, String>,
}

const LISTS: [List; 2] = [
    List {
        key: "themis",
        name: "Dayside magnetopause crossings, THEMIS 2007-2022 (Grimmich et al. 2023)",
        about: "The dayside magnetopause crossings that a random forest classifier found in the data of the five THEMIS probes, from 2007 to mid-2022 (section 3 of the paper): 184,292 crossings, each at its time (UT). `number` is the crossing's number in the file, `probe` the probe (tha to the) and `probability` the classifier's prediction probability. `r0_re` is the stand-off distance of the magnetopause during the crossing and `delta_r0_re` its deviation from the Shue et al. (1998) model, in Earth radii; both are left out when the file has none. `lat_deg`, `lon_deg` and `r_sc_re` place the probe (degrees, Earth radii).",
        credit: "Grimmich, Plaschke, Archer, Heyner, Mieth, Nakamura and Sibeck (2023), Study of extreme magnetopause distortions under varying solar wind conditions, JGR Space Physics 128, e2023JA031603, https://doi.org/10.1029/2023JA031603; dataset https://doi.org/10.17605/OSF.IO/B6KUX, licence CC-BY-4.0.",
        url: "https://osf.io/b6kux/",
        files: &[
            "https://files.de-1.osf.io/v1/resources/b6kux/providers/osfstorage/64217e9c3ed7dd1b8eecf99e",
        ],
        columns: &THEMIS_NAMES,
        read: |texts| themis(&texts[0]),
    },
    List {
        key: "cluster",
        name: "Magnetopause crossings, Cluster C1 and C3 2001-2020 (Grimmich et al. 2024)",
        about: "The magnetopause crossings that a random forest classifier found in the data of Cluster 1 and 3, from 2001 to 2020, at high latitudes too (section 2 of the paper): 38,322 crossings, each at its time (UT). `number` is the crossing's number in the file, `probe` C1 or C3, `probability` the classifier's prediction probability, `direction` inbound or outbound, and `data_quality` the mean CIS-HIA quality flag around the crossing (2 after 2014, when there are no flags). The spacecraft's AGSE position (`x_agse_re`, `y_agse_re`, `z_agse_re`, `r_sc_re`, Earth radii), latitude and longitude, magnetic latitude (SM) and magnetic local time follow, then `r0_re` and `delta_r0_re`, the stand-off distance and its deviation from the Shue et al. (1998) model, when known. 20,711 crossings also carry the analysis of the dataset's second file (sections 2-4): the tetrahedron's elongation and planarity (Robert et al. 1998); the magnetopause normal from multi-spacecraft timing (`normal_x`, `normal_y`, `normal_z`; Schwartz 1998), the cross-correlation coefficients it rests on (`xcorr_1` to `xcorr_3`) and the magnetopause speed (km/s); the angles of that normal to the Shue model's normal and to the magnetosheath field; the Walén test's slope and correlation (Paschmann and Sonnerup 2008); the dipole tilt; and OMNI's IMF and solar wind (`imf_*`, `sw_*`, AGSE). The file's nan and inf are left out. Where the two files give a different data quality (3 crossings, February 2013), the first file's is kept.",
        credit: "Grimmich, Plaschke, Grison, Prencipe, Escoubet, Archer, Constantinescu, Haaland, Nakamura, Sibeck, Darrouzet, Hayosh and Maggiolo (2024), The Cluster spacecrafts' view of the motion of the high-latitude magnetopause, Ann. Geophys. 42, 371-394, https://doi.org/10.5194/angeo-42-371-2024; dataset https://doi.org/10.17605/OSF.IO/PXCTG, licence CC-BY-4.0.",
        url: "https://osf.io/pxctg/",
        files: &[
            "https://files.de-1.osf.io/v1/resources/pxctg/providers/osfstorage/65ce2af7c3ab49024146b3f7",
            "https://files.de-1.osf.io/v1/resources/pxctg/providers/osfstorage/65e839e58992ec0529788a6c",
        ],
        columns: &CLUSTER_NAMES,
        read: |texts| cluster(&texts[0], &texts[1]),
    },
];

/// The file's column, and the attribute it is.
type Column = (&'static str, &'static str);

const THEMIS_COLUMNS: [Column; 8] = [
    ("No.", "number"),
    ("Probe", "probe"),
    ("Probability", "probability"),
    ("R0", "r0_re"),
    ("Delta_R0", "delta_r0_re"),
    ("Lat_ang", "lat_deg"),
    ("Lon_ang", "lon_deg"),
    ("r_sc", "r_sc_re"),
];
const THEMIS_NAMES: [&str; 8] = names(THEMIS_COLUMNS);

const CLUSTER_COLUMNS: [Column; 15] = [
    ("No.", "number"),
    ("Probe", "probe"),
    ("Probability", "probability"),
    ("Type", "direction"),
    ("DataQuality", "data_quality"),
    ("x_agse", "x_agse_re"),
    ("y_agse", "y_agse_re"),
    ("z_agse", "z_agse_re"),
    ("r_sc", "r_sc_re"),
    ("Lat_ang", "lat_deg"),
    ("Lon_ang", "lon_deg"),
    ("MLAT", "mlat_deg"),
    ("MLT", "mlt_h"),
    ("R0", "r0_re"),
    ("Delta_R0", "delta_r0_re"),
];
/// The second Cluster file's columns that hold one value; its number and data quality are
/// the first file's.
const ANALYSIS_COLUMNS: [Column; 24] = [
    ("ScGeomE", "geometry_elongation"),
    ("ScGeomP", "geometry_planarity"),
    ("MP_velo", "mp_speed_km_s"),
    ("DevAngSh98", "normal_vs_shue98_deg"),
    ("AngMPBsh", "normal_vs_sheath_field_deg"),
    ("Walen_Slp", "walen_slope"),
    ("Walen_Coeff", "walen_correlation"),
    ("Tilt", "dipole_tilt_deg"),
    ("Bx_agse", "imf_bx_nt"),
    ("By_agse", "imf_by_nt"),
    ("Bz_agse", "imf_bz_nt"),
    ("B_IMF", "imf_b_nt"),
    ("Cone_angle", "imf_cone_deg"),
    ("Clock_angle", "imf_clock_deg"),
    ("ux_agse", "sw_vx_km_s"),
    ("uy_agse", "sw_vy_km_s"),
    ("uz_agse", "sw_vz_km_s"),
    ("u_ion", "sw_v_km_s"),
    ("n_ion", "sw_n_cm3"),
    ("T_ion", "sw_t_k"),
    ("Dp", "sw_pdyn_npa"),
    ("Beta", "sw_beta"),
    ("M_MS", "sw_mach_ms"),
    ("M_A", "sw_mach_a"),
];
/// The second file's columns that hold a bracketed vector, `[ 0.17 -0.58 -0.8 ]`.
const VECTOR_COLUMNS: [(&str, [&str; 3]); 2] = [
    ("Normal", ["normal_x", "normal_y", "normal_z"]),
    ("Xcorr_Coeff", ["xcorr_1", "xcorr_2", "xcorr_3"]),
];
const CLUSTER_NAMES: [&str; 45] = {
    let mut all = [""; 45];
    let mut at = 0;
    while at < 15 {
        all[at] = CLUSTER_COLUMNS[at].1;
        at += 1;
    }
    let mut vector = 0;
    while vector < 2 {
        let mut axis = 0;
        while axis < 3 {
            all[at] = VECTOR_COLUMNS[vector].1[axis];
            at += 1;
            axis += 1;
        }
        vector += 1;
    }
    let mut column = 0;
    while column < 24 {
        all[at] = ANALYSIS_COLUMNS[column].1;
        at += 1;
        column += 1;
    }
    all
};

const fn names<const N: usize>(columns: [Column; N]) -> [&'static str; N] {
    let mut names = [""; N];
    let mut at = 0;
    while at < N {
        names[at] = columns[at].1;
        at += 1;
    }
    names
}

#[async_trait]
impl Provider for Grimmich {
    fn id(&self) -> &'static str {
        "grimmich"
    }

    fn source(&self) -> &'static str {
        "Magnetopause crossings, THEMIS and Cluster (Grimmich et al., OSF)"
    }

    fn default_schedule(&self) -> &'static str {
        "0 5 2 * *"
    }

    async fn fetch(
        &self,
        http: &reqwest::Client,
        _settings: &Value,
        _refreshed: &HashMap<String, Timestamp>,
    ) -> Result<Vec<Fetched>, String> {
        let mut lists = Vec::new();
        for list in &LISTS {
            let events = match texts(http, list.files).await {
                Ok(texts) => (list.read)(&texts),
                Err(error) => Err(error),
            };
            lists.push(fetched(list, events));
        }
        Ok(lists)
    }
}

async fn texts(http: &reqwest::Client, urls: &[&str]) -> Result<Vec<String>, String> {
    let mut texts = Vec::with_capacity(urls.len());
    for url in urls {
        let bytes = super::patient::fetch(http, url, BUSY_PAUSE).await?;
        texts.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    Ok(texts)
}

fn fetched(list: &List, events: Result<Vec<EventRecord>, String>) -> Fetched {
    Fetched {
        key: list.key.to_owned(),
        folder: FOLDER.to_owned(),
        name: list.name.to_owned(),
        description: format!("{} {KLINGENSTEIN} Cite: {}", list.about, list.credit),
        url: list.url.to_owned(),
        catalogue: Uuid::new_v5(&NAMESPACE, list.key.as_bytes()).to_string(),
        columns: list.columns.iter().map(|name| (*name).to_owned()).collect(),
        events,
    }
}

/// A file's header and rows, each row as many cells as the header; `#` lines are comments.
struct Table<'a> {
    header: Vec<&'a str>,
    rows: Vec<Vec<&'a str>>,
}

impl<'a> Table<'a> {
    fn read(text: &'a str) -> Result<Self, String> {
        let mut lines = text
            .lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty());
        let header: Vec<&str> = lines
            .next()
            .ok_or("no header, has the file layout changed?")?
            .split('\t')
            .map(str::trim)
            .collect();
        let rows = lines
            .map(|line| {
                let cells: Vec<&str> = line.split('\t').map(str::trim).collect();
                match cells.len() == header.len() {
                    true => Ok(cells),
                    false => Err(format!(
                        "{} cells, not {}: {line:?}",
                        cells.len(),
                        header.len()
                    )),
                }
            })
            .collect::<Result<_, String>>()?;
        Ok(Self { header, rows })
    }

    fn index(&self, name: &str) -> Result<usize, String> {
        self.header
            .iter()
            .position(|header| *header == name)
            .ok_or_else(|| format!("no {name} column, has the file layout changed?"))
    }

    /// Each column's index, with its attribute.
    fn indices(&self, columns: &[Column]) -> Result<Vec<(usize, &'static str)>, String> {
        columns
            .iter()
            .map(|(header, name)| Ok((self.index(header)?, *name)))
            .collect()
    }
}

/// The crossing's time and probe cells, the key of its row in either Cluster file.
fn key<'a>(table: &Table, row: &[&'a str]) -> Result<(&'a str, &'a str), String> {
    Ok((row[table.index("Timestamp")?], row[table.index("Probe")?]))
}

fn owned((time, probe): (&str, &str)) -> (String, String) {
    (time.to_owned(), probe.to_owned())
}

fn crossing_time(cell: &str) -> Result<Timestamp, String> {
    Timestamp::parse(&format!("{}Z", cell.replace(' ', "T")))
        .ok_or_else(|| format!("bad time: {cell:?}"))
}

fn keep(attributes: &mut Map<String, Value>, row: &[&str], indices: &[(usize, &str)]) {
    for (index, name) in indices {
        if let Some(value) = super::literature::value(row[*index]) {
            attributes.insert((*name).to_owned(), value);
        }
    }
}

/// Each number of a bracketed vector, `[ 0.17 -0.58 -0.8 ]`, as its axis' attribute; a nan,
/// or an axis the vector lacks, is left out.
fn keep_vector(attributes: &mut Map<String, Value>, cell: &str, names: &[&str; 3]) {
    let values = cell
        .trim_matches(|c| c == '[' || c == ']')
        .split_whitespace();
    for (name, text) in names.iter().zip(values) {
        if let Some(value) = super::literature::value(text) {
            attributes.insert((*name).to_owned(), value);
        }
    }
}

fn crossings(
    list: &str,
    table: &Table,
    columns: &[Column],
    mut more: impl FnMut((&str, &str), &mut Map<String, Value>) -> Result<(), String>,
) -> Result<Vec<EventRecord>, String> {
    let indices = table.indices(columns)?;
    let events = table
        .rows
        .iter()
        .map(|row| {
            let (time, probe) = key(table, row)?;
            let at = crossing_time(time)?;
            let mut event = EventRecord::new(event_uuid(list, probe, time), at, at);
            keep(&mut event.attributes, row, &indices);
            more((time, probe), &mut event.attributes)?;
            Ok(event)
        })
        .collect::<Result<Vec<_>, String>>()?;
    match events.is_empty() {
        true => Err("no crossing found, has the file layout changed?".to_owned()),
        false => Ok(events),
    }
}

fn themis(text: &str) -> Result<Vec<EventRecord>, String> {
    crossings(
        "themis",
        &Table::read(text)?,
        &THEMIS_COLUMNS,
        |_, _| Ok(()),
    )
}

/// The first file's crossings, each with the second file's analysis when it has one. An
/// analysis of no listed crossing fails the read: the two files no longer match.
fn cluster(crossings_text: &str, analysed: &str) -> Result<Vec<EventRecord>, String> {
    let mut analyses = analyses(&Table::read(analysed)?)?;
    let events = crossings(
        "cluster",
        &Table::read(crossings_text)?,
        &CLUSTER_COLUMNS,
        |key, attributes| {
            if let Some(analysis) = analyses.remove(&owned(key)) {
                attributes.extend(analysis);
            }
            Ok(())
        },
    )?;
    match analyses.keys().next() {
        Some((time, probe)) => Err(format!(
            "{} analyses of no listed crossing, the first {probe} at {time}",
            analyses.len()
        )),
        None => Ok(events),
    }
}

/// The second Cluster file's attributes, by crossing.
/// A Cluster crossing's attributes from the second file, by its time and probe cells.
type Analyses = HashMap<(String, String), Map<String, Value>>;

fn analyses(table: &Table) -> Result<Analyses, String> {
    let indices = table.indices(&ANALYSIS_COLUMNS)?;
    let vectors = VECTOR_COLUMNS
        .iter()
        .map(|(header, names)| Ok((table.index(header)?, names)))
        .collect::<Result<Vec<_>, String>>()?;
    table
        .rows
        .iter()
        .map(|row| {
            let mut attributes = Map::new();
            for (index, names) in &vectors {
                keep_vector(&mut attributes, row[*index], names);
            }
            keep(&mut attributes, row, &indices);
            Ok((owned(key(table, row)?), attributes))
        })
        .collect()
}

fn event_uuid(key: &str, probe: &str, time: &str) -> String {
    Uuid::new_v5(&NAMESPACE, format!("{key} {probe} {time}").as_bytes()).to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const THEMIS: &str = include_str!("../../tests/fixtures/relay/grimmich_themis.txt");
    const CLUSTER: &str = include_str!("../../tests/fixtures/relay/grimmich_cluster.txt");
    const ANALYSED: &str = include_str!("../../tests/fixtures/relay/grimmich_cluster_mst_sw.txt");

    fn at(text: &str) -> Timestamp {
        Timestamp::parse(text).unwrap()
    }

    fn find<'a>(events: &'a [EventRecord], time: &str, probe: &str) -> &'a EventRecord {
        events
            .iter()
            .find(|event| event.start == at(time) && event.attributes["probe"] == json!(probe))
            .unwrap()
    }

    #[test]
    fn a_themis_row_is_a_crossing_with_its_columns() {
        let events = themis(THEMIS).unwrap();

        assert_eq!(events.len(), 5);
        let first = &events[0];
        assert_eq!(first.start, at("2007-03-23T23:39:24Z"));
        assert_eq!(first.stop, first.start, "a crossing is an instant");
        let a = &first.attributes;
        assert_eq!(a["number"], json!(0.0));
        assert_eq!(a["probe"], json!("thd"));
        assert_eq!(a["probability"], json!(0.868));
        assert_eq!(a["r0_re"], json!(7.375));
        assert_eq!(a["delta_r0_re"], json!(-2.215));
        assert_eq!(a["lat_deg"], json!(-10.74));
        assert_eq!(a["lon_deg"], json!(85.182));
        assert_eq!(a["r_sc_re"], json!(10.503));
        assert_eq!(
            first.uuid,
            event_uuid("themis", "thd", "2007-03-23 23:39:24")
        );
    }

    #[test]
    fn a_themis_crossing_without_a_standoff_distance_leaves_it_out() {
        let events = themis(THEMIS).unwrap();
        let a = &find(&events, "2007-04-08T21:47:24Z", "tha").attributes;

        assert!(!a.contains_key("r0_re"));
        assert!(!a.contains_key("delta_r0_re"));
        assert_eq!(a["r_sc_re"], json!(14.495));
    }

    #[test]
    fn a_changed_layout_is_an_error() {
        assert!(themis("# no table\n").is_err());
        assert!(themis("No.\tTime\tProbe\n0\t2007-03-23 23:39:24\tthd\n").is_err());
        let short = THEMIS.replace("\t10.503\n", "\n");
        assert!(themis(&short).is_err(), "a row with a cell missing");
    }

    #[test]
    fn a_cluster_row_is_a_crossing_with_its_columns() {
        let events = cluster(CLUSTER, ANALYSED).unwrap();

        assert_eq!(events.len(), 7);
        let a = &find(&events, "2001-02-02T14:33:00Z", "C3").attributes;
        assert_eq!(a["probability"], json!(0.636));
        assert_eq!(a["data_quality"], json!(2.75));
        assert_eq!(a["direction"], json!("outbound"));
        assert_eq!(a["x_agse_re"], json!(6.962));
        assert_eq!(a["mlat_deg"], json!(44.117));
        assert_eq!(a["mlt_h"], json!(14.345));
        assert!(!a.contains_key("r0_re"), "empty in the file");
        assert!(!a.contains_key("normal_x"), "not in the second file");
    }

    #[test]
    fn a_cluster_crossing_gets_the_second_files_analysis() {
        let events = cluster(CLUSTER, ANALYSED).unwrap();
        let a = &find(&events, "2020-05-19T08:44:00Z", "C1").attributes;

        assert_eq!(a["r0_re"], json!(9.015));
        assert_eq!(a["geometry_elongation"], json!(0.689));
        assert_eq!(a["normal_x"], json!(0.03));
        assert_eq!(a["normal_y"], json!(-1.0));
        assert_eq!(a["normal_z"], json!(0.05));
        assert_eq!(a["xcorr_3"], json!(0.731));
        assert_eq!(a["mp_speed_km_s"], json!(-81.69));
        assert_eq!(a["walen_correlation"], json!(-0.758));
        assert_eq!(a["imf_bz_nt"], json!(-1.073));
        assert_eq!(a["sw_t_k"], json!(79439.11));
        assert_eq!(a["sw_mach_a"], json!(8.078));
    }

    #[test]
    fn nan_inf_and_missing_values_of_the_analysis_are_left_out() {
        let events = cluster(CLUSTER, ANALYSED).unwrap();
        let unsolved = &find(&events, "2003-10-24T15:25:00Z", "C1").attributes;
        assert!(!unsolved.contains_key("normal_x"), "nan");
        assert!(!unsolved.contains_key("mp_speed_km_s"), "inf");
        assert_eq!(unsolved["xcorr_1"], json!(0.985));
        let two = &find(&events, "2001-02-16T20:01:00Z", "C3").attributes;
        assert_eq!(two["xcorr_2"], json!(0.959));
        assert!(!two.contains_key("xcorr_3"), "two coefficients only");
        let early = &find(&events, "2001-02-04T21:41:00Z", "C1").attributes;
        assert!(!early.contains_key("imf_bz_nt"), "no OMNI data");
    }

    #[test]
    fn an_analysis_of_no_listed_crossing_is_an_error() {
        let lone = CLUSTER
            .lines()
            .filter(|line| !line.contains("2020-05-19 08:44:00"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(cluster(&lone, ANALYSED).is_err());
    }
}
