//! Files in and out of a project: the export of catalogues in a format, and what a file
//! brought in holds and will change.

use std::collections::BTreeSet;

use cocat_core::model::{
    self, CatalogueSummary, EventRecord, Member, ShapeAxis, WriteError, Written,
};
use cocat_core::rules::RuleError;
use cocat_core::tfcat::{self, TfcatError, TfcatImport};
use cocat_core::time::Timestamp;
use cocat_core::tscat_json::{self, TscatImport, TscatJsonError};
use cocat_core::votable::{self, Imported, VotableError};
use serde_json::Value;
use thiserror::Error;
use yrs::{ReadTxn, TransactionMut};

#[derive(Debug, Error)]
pub enum TransferError {
    #[error("this catalogue is no longer in the project")]
    NoCatalogue,
    #[error("the rule does not evaluate: {0}")]
    Rule(#[from] RuleError),
    #[error(transparent)]
    Votable(#[from] VotableError),
    #[error(transparent)]
    Tscat(#[from] TscatJsonError),
    #[error(transparent)]
    Tfcat(#[from] TfcatError),
    #[error("cannot write the file: {0}")]
    Encoding(String),
    #[error("This file is not a VOTable (.xml, .vot), tscat JSON or TFCat (.json).")]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Votable,
    TscatJson,
    Tfcat,
    Csv,
}

impl Format {
    pub const ALL: [Format; 4] = [
        Format::Votable,
        Format::TscatJson,
        Format::Tfcat,
        Format::Csv,
    ];
    /// The formats the project's export offers for several catalogues.
    pub const MANY: [Format; 3] = [Format::Votable, Format::TscatJson, Format::Tfcat];

    pub fn label(self) -> &'static str {
        match self {
            Format::Votable => "VOTable (.xml)",
            Format::TscatJson => "tscat JSON (.json)",
            Format::Tfcat => "TFCat, radio time–frequency (.json)",
            Format::Csv => "CSV (.csv)",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Votable => "xml",
            Format::TscatJson | Format::Tfcat => "json",
            Format::Csv => "csv",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Format::Votable => "application/x-votable+xml",
            Format::TscatJson | Format::Tfcat => "application/json",
            Format::Csv => "text/csv",
        }
    }
}

/// The files an export makes, as (file name, content): one, except for TFCat, which holds
/// one catalogue per file.
pub fn files<T: ReadTxn>(
    txn: &T,
    uuids: &[String],
    format: Format,
    value_range: Option<(f64, f64)>,
) -> Result<Vec<(String, String)>, TransferError> {
    let groups: Vec<&[String]> = match format {
        Format::Tfcat => uuids.chunks(1).collect(),
        _ => vec![uuids],
    };
    groups
        .into_iter()
        .map(|uuids| {
            let names: Vec<String> = uuids
                .iter()
                .filter_map(|uuid| model::catalogue(txn, uuid).map(|found| found.name))
                .collect();
            let text = export(txn, uuids, format, value_range)?;
            Ok((file_name(&names, format), text))
        })
        .collect()
}

/// The shape axis of the first catalogue whose TFCat export needs a range of values from
/// the visitor, or `None` when none does.
pub fn needs_range<T: ReadTxn>(txn: &T, uuids: &[String]) -> Option<ShapeAxis> {
    uuids.iter().find_map(|uuid| {
        let catalogue = model::catalogue(txn, uuid)?;
        let members = model::catalogue_members(txn, uuid)?.ok()?;
        tfcat::needs_value_range(&events_of(&members)).then(|| tfcat::shape_axis(&catalogue))
    })
}

/// What a TFCat export of these catalogues says before it writes them: one line per
/// catalogue whose shapes TFCat does not know.
pub fn tfcat_warnings<T: ReadTxn>(txn: &T, uuids: &[String]) -> Vec<String> {
    uuids
        .iter()
        .filter_map(|uuid| model::catalogue(txn, uuid))
        .filter_map(|catalogue| tfcat::strictness_warning(&catalogue))
        .collect()
}

/// The range typed in two boxes: `None` when both are empty.
pub fn value_range(low: &str, high: &str) -> Result<Option<(f64, f64)>, String> {
    let refused = || "Give two numbers, the lowest first.".to_owned();
    match (low.trim(), high.trim()) {
        ("", "") => Ok(None),
        (low, high) => match (low.parse::<f64>(), high.parse::<f64>()) {
            (Ok(low), Ok(high)) if low < high => Ok(Some((low, high))),
            _ => Err(refused()),
        },
    }
}

/// The catalogues in one file. A smart catalogue exports its current members.
pub fn export<T: ReadTxn>(
    txn: &T,
    uuids: &[String],
    format: Format,
    value_range: Option<(f64, f64)>,
) -> Result<String, TransferError> {
    let mut exported = Vec::with_capacity(uuids.len());
    for uuid in uuids {
        let catalogue = model::catalogue(txn, uuid).ok_or(TransferError::NoCatalogue)?;
        let members = model::catalogue_members(txn, uuid).ok_or(TransferError::NoCatalogue)??;
        exported.push((catalogue, members));
    }
    let events: Vec<(&CatalogueSummary, Vec<EventRecord>)> = exported
        .iter()
        .map(|(catalogue, members)| (catalogue, events_of(members)))
        .collect();
    let pairs: Vec<(&CatalogueSummary, &[EventRecord])> = events
        .iter()
        .map(|(catalogue, events)| (*catalogue, events.as_slice()))
        .collect();
    match format {
        Format::Votable => Ok(votable::export_many(&pairs)?),
        Format::TscatJson => tscat_json::export(txn, &pairs).map_err(encoding),
        Format::Tfcat => match pairs.as_slice() {
            [(catalogue, events)] => Ok(tfcat::export(txn, catalogue, events, value_range)?),
            _ => Err(TransferError::Encoding(
                "a TFCat file holds one catalogue".to_owned(),
            )),
        },
        Format::Csv => {
            let members = exported.iter().flat_map(|(_, members)| members.iter());
            csv(txn, members)
        }
    }
}

fn events_of(members: &[Member]) -> Vec<EventRecord> {
    members
        .iter()
        .filter_map(|member| match member {
            Member::Event { record, .. } => Some(record.clone()),
            Member::Derived { .. } => None,
        })
        .collect()
}

const CSV_FIXED: [&str; 7] = [
    "start",
    "stop",
    "tags",
    "status",
    "confidence",
    "author",
    "comment",
];

/// One row per member, in the table's order: a derived interval has only its times.
fn csv<'m, T: ReadTxn>(
    txn: &T,
    members: impl Iterator<Item = &'m Member> + Clone,
) -> Result<String, TransferError> {
    let keys: BTreeSet<&String> = members
        .clone()
        .filter_map(|member| match member {
            Member::Event { record, .. } => Some(record.attributes.keys()),
            Member::Derived { .. } => None,
        })
        .flatten()
        .collect();
    let mut writer = csv::Writer::from_writer(Vec::new());
    let header = CSV_FIXED
        .into_iter()
        .chain(keys.iter().map(|key| key.as_str()));
    writer.write_record(header).map_err(encoding)?;
    for member in members {
        writer
            .write_record(csv_row(txn, member, &keys))
            .map_err(encoding)?;
    }
    let bytes = writer.into_inner().map_err(encoding)?;
    String::from_utf8(bytes).map_err(encoding)
}

fn csv_row<T: ReadTxn>(txn: &T, member: &Member, keys: &BTreeSet<&String>) -> Vec<String> {
    let (start, stop) = match member {
        Member::Event { record, .. } => (record.start, record.stop),
        Member::Derived { start, stop, .. } => (*start, *stop),
    };
    let times = [start.to_string(), stop.to_string()];
    let Member::Event { record, .. } = member else {
        let width = CSV_FIXED.len() + keys.len();
        let mut row: Vec<String> = times.into_iter().collect();
        row.resize(width, String::new());
        return row;
    };
    let fixed = [
        record.tags.join(";"),
        record.status.name().to_owned(),
        record
            .confidence
            .map(|value| value.to_string())
            .unwrap_or_default(),
        record.author.clone().unwrap_or_default(),
        model::comment(txn, &record.uuid).unwrap_or_default(),
    ];
    let attributes = keys
        .iter()
        .map(|key| record.attributes.get(*key).map(cell).unwrap_or_default());
    times.into_iter().chain(fixed).chain(attributes).collect()
}

fn cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn encoding(error: impl std::fmt::Display) -> TransferError {
    TransferError::Encoding(error.to_string())
}

/// The file name for catalogues: the one catalogue's name, or "catalogues", made safe for
/// every file system.
pub fn file_name(names: &[String], format: Format) -> String {
    let base = match names {
        [one] => sanitise(one),
        _ => String::new(),
    };
    let base = if base.is_empty() {
        "catalogues".to_owned()
    } else {
        base
    };
    format!("{base}.{}", format.extension())
}

/// Letters, digits, `-`, `_` and `.` stay; any other run of characters becomes one `_`.
fn sanitise(name: &str) -> String {
    let mut safe = String::with_capacity(name.len());
    for character in name.trim().chars() {
        if character.is_alphanumeric() || matches!(character, '-' | '_' | '.') {
            safe.push(character);
        } else if !safe.ends_with('_') {
            safe.push('_');
        }
    }
    safe.trim_matches(['_', '.']).chars().take(100).collect()
}

/// A file read, before anything is written.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Votable(Imported),
    Tscat(TscatImport),
    /// A TFCat file, with the name of the catalogue it makes.
    Tfcat(TfcatImport, String),
}

/// Reads a file by what it holds, whatever its name says. A TFCat file without a title
/// names its catalogue after `file_name`.
pub fn read(text: &str, file_name: &str) -> Result<Incoming, TransferError> {
    let text = text.trim_start_matches('\u{feff}').trim_start();
    match text.chars().next() {
        Some('<') => Ok(Incoming::Votable(votable::import(text)?)),
        Some('{') if is_feature_collection(text) => {
            let import = tfcat::parse(text)?;
            let name = import.name(None, file_name);
            Ok(Incoming::Tfcat(import, name))
        }
        Some('{') => Ok(Incoming::Tscat(tscat_json::parse(text)?)),
        _ => Err(TransferError::Unknown),
    }
}

/// TFCat is GeoJSON: a `FeatureCollection`. tscat's JSON has no `type`.
fn is_feature_collection(text: &str) -> bool {
    #[derive(serde::Deserialize)]
    struct Typed {
        #[serde(rename = "type")]
        kind: Option<String>,
    }
    serde_json::from_str::<Typed>(text)
        .is_ok_and(|typed| typed.kind.as_deref() == Some("FeatureCollection"))
}

/// What an import will do to the project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub new_catalogues: usize,
    pub updated_catalogues: usize,
    pub new_events: usize,
    pub updated_events: usize,
}

pub fn summary<T: ReadTxn>(txn: &T, incoming: &Incoming) -> Summary {
    let (catalogues, events): (Vec<Option<&str>>, Vec<&EventRecord>) = match incoming {
        Incoming::Votable(imported) => (
            imported.tables.iter().map(|_| None).collect(),
            imported
                .tables
                .iter()
                .flat_map(|table| &table.events)
                .collect(),
        ),
        Incoming::Tscat(import) => (
            import
                .catalogues
                .iter()
                .map(|catalogue| Some(catalogue.uuid.as_str()))
                .collect(),
            import.events.iter().collect(),
        ),
        Incoming::Tfcat(import, _) => (vec![None], import.events.iter().collect()),
    };
    let uuids: BTreeSet<&str> = events.iter().map(|event| event.uuid.as_str()).collect();
    let updated_events = uuids
        .iter()
        .filter(|uuid| model::event(txn, uuid).is_some())
        .count();
    let updated_catalogues = catalogues
        .iter()
        .flatten()
        .filter(|uuid| model::catalogue(txn, uuid).is_some())
        .count();
    Summary {
        new_catalogues: catalogues.len() - updated_catalogues,
        updated_catalogues,
        new_events: uuids.len() - updated_events,
        updated_events,
    }
}

impl Summary {
    /// "Adds 2 catalogues and 300 events, updates 5 events (same ids)."
    pub fn text(&self) -> String {
        let adds = both(self.new_catalogues, self.new_events);
        let updates = both(self.updated_catalogues, self.updated_events);
        match (adds, updates) {
            (None, None) => "The file holds no catalogue and no event.".to_owned(),
            (Some(adds), None) => format!("Adds {adds}."),
            (None, Some(updates)) => format!("Updates {updates} (same ids)."),
            (Some(adds), Some(updates)) => format!("Adds {adds}, updates {updates} (same ids)."),
        }
    }
}

fn both(catalogues: usize, events: usize) -> Option<String> {
    let parts: Vec<String> = [(catalogues, "catalogue"), (events, "event")]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, noun)| count_of(count, noun))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" and "))
}

fn count_of(count: usize, noun: &str) -> String {
    let plural = if count == 1 { "" } else { "s" };
    format!("{count} {noun}{plural}")
}

/// What the file says could not come across as it was.
pub fn warnings(incoming: &Incoming) -> &[String] {
    match incoming {
        Incoming::Votable(_) => &[],
        Incoming::Tscat(import) => &import.warnings,
        Incoming::Tfcat(import, _) => &import.warnings,
    }
}

/// Writes the file in the caller's transaction, so the import is one change.
pub fn write(
    txn: &mut TransactionMut<'_>,
    incoming: &Incoming,
    now: Timestamp,
) -> Result<Written, WriteError> {
    match incoming {
        Incoming::Votable(imported) => votable::write(txn, imported, None),
        Incoming::Tscat(import) => tscat_json::write(txn, import, now),
        Incoming::Tfcat(import, name) => tfcat::write(txn, import, name, now),
    }
}

#[cfg(test)]
mod tests;
