//! Events as a CSV table, one row per event. The export is what a spreadsheet opens. The
//! import reads that file back, or any table with a start and a stop column: the columns it
//! knows become the event's fields, the others its attributes.

use std::path::Path;

use serde_json::{Number, Value};
use thiserror::Error;
use yrs::{ReadTxn, TransactionMut};

use crate::model::{self, EventRecord, Member, Status, WriteError, Written};
use crate::time::Timestamp;
use crate::votable::{self, Imported, ImportedTable};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Start,
    Stop,
    Tags,
    Status,
    Confidence,
    Author,
    Comment,
    Uuid,
    Attribute,
}

/// The fixed columns, in the export's order, with the lower-case names the import also
/// takes for them; the first name is the one the export writes.
const COLUMNS: [(Field, &[&str]); 8] = [
    (
        Field::Start,
        &["start", "start time", "start_time", "begin"],
    ),
    (
        Field::Stop,
        &[
            "stop",
            "stop time",
            "stop_time",
            "end",
            "end time",
            "end_time",
        ],
    ),
    (Field::Tags, &["tags"]),
    (Field::Status, &["status"]),
    (Field::Confidence, &["confidence"]),
    (Field::Author, &["author"]),
    (Field::Comment, &["comment"]),
    (Field::Uuid, &["uuid"]),
];

const AUTHOR: &str = "CSV import";
const UNNAMED: &str = "Imported catalogue";

#[derive(Debug, Error)]
pub enum CsvError {
    #[error("The file has no {0} column. A CSV file needs one start and one stop column.")]
    Missing(&'static str),
    #[error("Row {row}: the {column} “{value}” is not a time, such as 2020-01-31T10:00:00Z.")]
    Time {
        row: usize,
        column: &'static str,
        value: String,
    },
    #[error("cannot read the CSV file: {0}")]
    Csv(#[from] csv::Error),
    #[error("cannot write the CSV file: {0}")]
    Encoding(String),
}

/// A CSV file read, before anything is written.
#[derive(Debug, Clone, PartialEq)]
pub struct CsvImport {
    /// The catalogue's name: the file's name without its extension.
    pub name: String,
    /// The columns carried as attributes, in the file's order.
    pub columns: Vec<String>,
    pub events: Vec<EventRecord>,
    /// `(event uuid, comment)`: comments live beside the events, not in them.
    pub comments: Vec<(String, String)>,
    pub warnings: Vec<String>,
}

/// Reads the table. The delimiter is the one the header uses most: `,`, `;` or a tab.
pub fn parse(text: &str, file_name: &str) -> Result<CsvImport, CsvError> {
    let text = text.trim_start_matches('\u{feff}');
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter(text))
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(text.as_bytes());
    let headers = reader.headers()?.clone();
    let fields: Vec<Field> = headers.iter().map(field).collect();
    for (wanted, name) in [(Field::Start, "start"), (Field::Stop, "stop")] {
        if !fields.contains(&wanted) {
            return Err(CsvError::Missing(name));
        }
    }
    let mut import = CsvImport {
        name: name_of(file_name),
        columns: headers
            .iter()
            .zip(&fields)
            .filter(|(_, field)| **field == Field::Attribute)
            .map(|(header, _)| header.to_owned())
            .collect(),
        events: Vec::new(),
        comments: Vec::new(),
        warnings: Vec::new(),
    };
    for (index, row) in reader.records().enumerate() {
        let row = row?;
        let cells: Vec<(&str, Field, &str)> = headers
            .iter()
            .zip(&fields)
            .zip(row.iter())
            .map(|((header, field), value)| (header, *field, value))
            .collect();
        // The header is the first line, so the first event is on the second.
        read_row(&mut import, index + 2, &cells)?;
    }
    Ok(import)
}

fn read_row(
    import: &mut CsvImport,
    row: usize,
    cells: &[(&str, Field, &str)],
) -> Result<(), CsvError> {
    let cell = |wanted: Field| {
        cells
            .iter()
            .find(|(_, field, _)| *field == wanted)
            .map_or("", |(_, _, value)| *value)
    };
    let time = |wanted: Field, column: &'static str| {
        Timestamp::parse(cell(wanted)).ok_or_else(|| CsvError::Time {
            row,
            column,
            value: cell(wanted).to_owned(),
        })
    };
    let uuid = match cell(Field::Uuid) {
        "" => uuid::Uuid::new_v4().to_string(),
        given => given.to_owned(),
    };
    let mut event = EventRecord::new(
        uuid,
        time(Field::Start, "start")?,
        time(Field::Stop, "stop")?,
    );
    for (header, field, value) in cells.iter().filter(|(_, _, value)| !value.is_empty()) {
        match field {
            Field::Tags => event.tags = tags(value),
            Field::Status => match Status::from_name(&value.to_lowercase()) {
                Some(status) => event.status = status,
                None => import.warnings.push(format!(
                    "Row {row}: the status “{value}” is not preliminary, confirmed, reviewed, final or rejected; it was left out."
                )),
            },
            Field::Confidence => match value.parse::<f64>() {
                Ok(confidence) if (0.0..=1.0).contains(&confidence) => {
                    event.confidence = Some(confidence);
                }
                _ => import.warnings.push(format!(
                    "Row {row}: the confidence “{value}” is not a number from 0 to 1; it was left out."
                )),
            },
            Field::Author => event.author = Some((*value).to_owned()),
            Field::Comment => import.comments.push((event.uuid.clone(), (*value).to_owned())),
            Field::Attribute => {
                event.attributes.insert((*header).to_owned(), typed(value));
            }
            Field::Start | Field::Stop | Field::Uuid => {}
        }
    }
    import.events.push(event);
    Ok(())
}

/// Writes the table as a new catalogue, in the caller's transaction. An event whose uuid the
/// project holds is replaced, as a VOTable import does.
pub fn write(
    txn: &mut TransactionMut<'_>,
    import: &CsvImport,
    name: Option<&str>,
) -> Result<Written, WriteError> {
    let imported = Imported {
        author: Some(AUTHOR.to_owned()),
        name: None,
        tables: vec![ImportedTable {
            name: name.unwrap_or(&import.name).to_owned(),
            shape_axis: None,
            columns: import.columns.clone(),
            events: import.events.clone(),
        }],
    };
    let written = votable::write(txn, &imported, None)?;
    for (uuid, comment) in &import.comments {
        model::set_comment(txn, uuid, Some(comment));
    }
    Ok(written)
}

/// One row per member, in the given order; an interval a rule made has only its times.
pub fn export<'m, T: ReadTxn>(
    txn: &T,
    members: impl Iterator<Item = &'m Member> + Clone,
) -> Result<String, CsvError> {
    let keys: std::collections::BTreeSet<&String> = members
        .clone()
        .filter_map(|member| match member {
            Member::Event { record, .. } => Some(record.attributes.keys()),
            Member::Derived { .. } => None,
        })
        .flatten()
        .collect();
    let mut writer = csv::Writer::from_writer(Vec::new());
    let header = COLUMNS
        .iter()
        .map(|(_, names)| names[0])
        .chain(keys.iter().map(|key| key.as_str()));
    writer.write_record(header)?;
    for member in members {
        writer.write_record(row(txn, member, &keys))?;
    }
    let bytes = writer.into_inner().map_err(encoding)?;
    String::from_utf8(bytes).map_err(encoding)
}

fn row<T: ReadTxn>(
    txn: &T,
    member: &Member,
    keys: &std::collections::BTreeSet<&String>,
) -> Vec<String> {
    let record = match member {
        Member::Event { record, .. } => record,
        Member::Derived { start, stop, .. } => {
            let mut row = vec![start.to_string(), stop.to_string()];
            row.resize(COLUMNS.len() + keys.len(), String::new());
            return row;
        }
    };
    let fixed = [
        record.start.to_string(),
        record.stop.to_string(),
        record.tags.join(";"),
        record.status.name().to_owned(),
        record
            .confidence
            .map(|value| value.to_string())
            .unwrap_or_default(),
        record.author.clone().unwrap_or_default(),
        model::comment(txn, &record.uuid).unwrap_or_default(),
        record.uuid.clone(),
    ];
    let attributes = keys
        .iter()
        .map(|key| record.attributes.get(*key).map(cell).unwrap_or_default());
    fixed.into_iter().chain(attributes).collect()
}

fn cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn encoding(error: impl std::fmt::Display) -> CsvError {
    CsvError::Encoding(error.to_string())
}

fn field(header: &str) -> Field {
    let name = header.trim().to_lowercase();
    COLUMNS
        .iter()
        .find(|(_, names)| names.contains(&name.as_str()))
        .map_or(Field::Attribute, |(field, _)| *field)
}

/// A tie goes to the comma.
fn delimiter(text: &str) -> u8 {
    let header = text.lines().next().unwrap_or_default();
    b"\t;,"
        .iter()
        .copied()
        .max_by_key(|delimiter| header.bytes().filter(|byte| byte == delimiter).count())
        .unwrap_or(b',')
}

/// Tags are separated by `;` as the export writes them, or by `,` as people type them.
fn tags(text: &str) -> Vec<String> {
    text.split([';', ','])
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A cell as a number or a boolean when it reads as one, else as text.
fn typed(text: &str) -> Value {
    if let Ok(integer) = text.parse::<i64>() {
        return Value::from(integer);
    }
    if let Some(number) = text.parse::<f64>().ok().and_then(Number::from_f64) {
        return Value::Number(number);
    }
    match text.to_lowercase().as_str() {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => Value::String(text.to_owned()),
    }
}

fn name_of(file_name: &str) -> String {
    Path::new(file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::trim)
        .filter(|stem| !stem.is_empty())
        .unwrap_or(UNNAMED)
        .to_owned()
}

#[cfg(test)]
mod tests;
