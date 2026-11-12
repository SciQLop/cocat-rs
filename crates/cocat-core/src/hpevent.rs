//! Events as a SPASE HPEvent list (the Heliophysics Event List specification,
//! https://spase-group.org/docs/conventions/HP-Event-List-Specification.pdf): `#Key: value`
//! header lines naming the list and its fields (`FieldNames`, `FieldUnits`, `FieldNulls`),
//! then one event per line, its start and stop first. Cells are separated by tabs, strings
//! quoted, or in older lists by spaces. CDAWeb's event lists, the STEREO Science Center's and
//! the Heliophysics Data Resource Library's catalogs are written this way.

use serde_json::{Map, Value};
use thiserror::Error;
use yrs::{ReadTxn, TransactionMut};

use crate::model::{Member, WriteError, Written};
use crate::time::Timestamp;

#[derive(Debug, Error)]
pub enum HpEventError {
    #[error("The file has no FieldNames line: it is not an HPEvent list.")]
    NoFieldNames,
    #[error("Line {row}: {why}")]
    Row { row: usize, why: String },
}

/// A field after the start and the stop, with its unit when the list gives one.
#[derive(Debug, Clone, PartialEq)]
pub struct HpEventField {
    pub name: String,
    pub unit: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HpEvent {
    pub start: Timestamp,
    pub stop: Timestamp,
    pub attributes: Map<String, Value>,
}

/// A list read, its header and its events.
#[derive(Debug, Clone, PartialEq)]
pub struct HpEventList {
    pub title: Option<String>,
    pub list_id: Option<String>,
    pub contact: Option<String>,
    pub description: Option<String>,
    pub fields: Vec<HpEventField>,
    pub events: Vec<HpEvent>,
}

/// An HPEvent file read for an import, before anything is written.
#[derive(Debug, Clone, PartialEq)]
pub struct HpEventImport {
    pub name: String,
    pub description: Option<String>,
    pub fields: Vec<HpEventField>,
    pub events: Vec<crate::model::EventRecord>,
}

const VERSION: &str =
    "HPEvent 1.0.4 https://spase-group.org/docs/conventions/HP-Event-List-Specification.pdf";
/// The units that say a field has none.
const NO_UNIT: [&str; 6] = ["none", "datetime", "text", "char", "string", ""];
const UNNAMED: &str = "Imported list";

/// One `#Key: value` header line, the key a single word.
fn header_line(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once(':')?;
    let key = key.trim();
    (!key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .then(|| (key, value.trim()))
}

/// The header's values by key, and the `Description`: its value and the comment lines after
/// it, up to the next key.
fn header(text: &str) -> (Vec<(String, String)>, Option<String>) {
    let mut keys = Vec::new();
    let mut description: Option<Vec<String>> = None;
    let mut in_description = false;
    for line in text.lines().filter_map(|line| line.strip_prefix('#')) {
        let line = line.trim();
        match header_line(line) {
            Some(("Description", value)) => {
                in_description = true;
                description = Some(if value.is_empty() {
                    Vec::new()
                } else {
                    vec![value.to_owned()]
                });
            }
            Some((key, value)) => {
                in_description = false;
                keys.push((key.to_owned(), value.to_owned()));
            }
            None if in_description && !line.is_empty() => {
                if let Some(description) = description.as_mut() {
                    description.push(line.to_owned());
                }
            }
            None => {}
        }
    }
    let description = description
        .filter(|lines| !lines.is_empty())
        .map(|lines| lines.join("\n"));
    (keys, description)
}

/// Whether `cell` is the field's null: the same text, or the same number (`-1.00` for `-1`).
fn is_null(null: &str, cell: &str) -> bool {
    null == cell
        || matches!((null.parse::<f64>(), cell.parse::<f64>()), (Ok(null), Ok(cell)) if null == cell)
}

/// A cell as a value: an integer, a number, a boolean or the text.
fn typed(text: &str) -> Value {
    if let Ok(integer) = text.parse::<i64>() {
        return Value::from(integer);
    }
    if let Some(number) = text
        .parse::<f64>()
        .ok()
        .and_then(serde_json::Number::from_f64)
    {
        return Value::Number(number);
    }
    match text.to_lowercase().as_str() {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => Value::String(text.to_owned()),
    }
}

/// A row's cells: tab-separated with quoted strings (which may hold tabs), or, in a list with
/// no tab, separated by spaces.
fn cells(line: &str) -> Vec<String> {
    if !line.contains('\t') {
        return line.split_whitespace().map(str::to_owned).collect();
    }
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b'\t')
        .has_headers(false)
        .flexible(true)
        .from_reader(line.as_bytes());
    reader
        .records()
        .next()
        .and_then(Result::ok)
        .map(|record| record.iter().map(|cell| cell.trim().to_owned()).collect())
        .unwrap_or_default()
}

/// Reads a list. `names` replaces the header's `FieldNames`, for a list whose header
/// miscounts them (`SIR_end time` in one of Lan Jian's lists).
pub fn read(text: &str, names: Option<&[&str]>) -> Result<HpEventList, HpEventError> {
    let text = text.trim_start_matches('\u{feff}');
    let (keys, description) = header(text);
    let value = |key: &str| {
        keys.iter()
            .find(|(known, _)| known == key)
            .map(|(_, value)| value.clone())
            .filter(|value| !value.is_empty())
    };
    let words = |key: &str| -> Vec<String> {
        value(key)
            .map(|line| line.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default()
    };
    let names: Vec<String> = match names {
        Some(names) => names.iter().map(|name| (*name).to_owned()).collect(),
        None => words("FieldNames")
            .into_iter()
            .map(|name| name.trim_start_matches('#').to_owned())
            .collect(),
    };
    if names.len() < 2 {
        return Err(HpEventError::NoFieldNames);
    }
    let units = words("FieldUnits");
    let nulls = words("FieldNulls");
    let mut events = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let row = index + 1;
        let cells = cells(line);
        let bad = |why: String| HpEventError::Row { row, why };
        let start = cells
            .first()
            .and_then(|cell| Timestamp::parse(cell))
            .ok_or_else(|| {
                bad(format!(
                    "the start “{}” is not a time",
                    cells.first().map_or("", String::as_str)
                ))
            })?;
        let stop = cells
            .get(1)
            .filter(|cell| !cell.starts_with("0001-"))
            .and_then(|cell| Timestamp::parse(cell))
            .map_or(start, |stop| stop.max(start));
        let mut attributes = Map::new();
        for (index, (name, cell)) in names.iter().zip(&cells).enumerate().skip(2) {
            let null = nulls.get(index).is_some_and(|null| is_null(null, cell));
            if cell.is_empty() || cell == "NULL" || null {
                continue;
            }
            attributes.insert(name.clone(), typed(cell));
        }
        events.push(HpEvent {
            start,
            stop,
            attributes,
        });
    }
    let fields = names
        .iter()
        .enumerate()
        .skip(2)
        .map(|(index, name)| HpEventField {
            name: name.clone(),
            unit: units
                .get(index)
                .filter(|unit| !NO_UNIT.contains(&unit.to_lowercase().as_str()))
                .cloned(),
        })
        .collect();
    Ok(HpEventList {
        title: value("ListTitle"),
        list_id: value("ListID"),
        contact: value("Contact"),
        description,
        fields,
        events,
    })
}

/// Reads a file to import: the catalogue takes the list's title, or the file's name.
pub fn parse(text: &str, file_name: &str) -> Result<HpEventImport, HpEventError> {
    let list = read(text, None)?;
    let name = list.title.clone().unwrap_or_else(|| {
        std::path::Path::new(file_name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::trim)
            .filter(|stem| !stem.is_empty())
            .unwrap_or(UNNAMED)
            .to_owned()
    });
    let events = list
        .events
        .into_iter()
        .map(|event| {
            let mut record = crate::model::EventRecord::new(
                uuid::Uuid::new_v4().to_string(),
                event.start,
                event.stop,
            );
            record.attributes = event.attributes;
            record
        })
        .collect();
    Ok(HpEventImport {
        name,
        description: list.description,
        fields: list.fields,
        events,
    })
}

/// Writes the import as a new catalogue: its fields' units go to the attribute registry,
/// the list's description to the catalogue's.
pub fn write(
    txn: &mut TransactionMut<'_>,
    import: &HpEventImport,
    name: Option<&str>,
) -> Result<Written, WriteError> {
    let imported = crate::votable::Imported {
        author: Some("HPEvent import".to_owned()),
        name: None,
        tables: vec![crate::votable::ImportedTable {
            name: name.unwrap_or(&import.name).to_owned(),
            shape_axis: None,
            columns: import
                .fields
                .iter()
                .map(|field| field.name.clone())
                .collect(),
            events: import.events.clone(),
        }],
    };
    let written = crate::votable::write(txn, &imported, None)?;
    for field in &import.fields {
        if let Some(unit) = &field.unit {
            crate::model::set_attribute_info(txn, &field.name, Some(unit), None);
        }
    }
    if let (Some(description), Some(catalogue)) = (&import.description, written.catalogues.first())
    {
        let attributes =
            Map::from_iter([("description".to_owned(), Value::from(description.as_str()))]);
        crate::model::set_catalogue_attributes(txn, catalogue, &attributes)?;
    }
    Ok(written)
}

/// A cell as the list writes it: a string in quotes, a missing value as `NULL`.
fn written_cell(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "NULL".to_owned(),
        Some(Value::String(text)) => format!("\"{}\"", text.replace('"', "'")),
        Some(other) => other.to_string(),
    }
}

/// The members as an HPEvent list titled `title`: start, stop, then every attribute, its
/// unit from the attribute registry; tab-separated, a missing value written `NULL`.
pub fn export<'m, T: ReadTxn>(
    txn: &T,
    members: impl Iterator<Item = &'m Member> + Clone,
    title: &str,
) -> String {
    let keys: std::collections::BTreeSet<&String> = members
        .clone()
        .filter_map(|member| match member {
            Member::Event { record, .. } => Some(record.attributes.keys()),
            Member::Derived { .. } => None,
        })
        .flatten()
        .collect();
    let registry = crate::model::attribute_types(txn);
    let unit = |key: &String| {
        registry
            .get(key.as_str())
            .and_then(|info| info.unit.clone())
            .filter(|unit| !unit.contains(char::is_whitespace))
            .unwrap_or_else(|| "None".to_owned())
    };
    let names: Vec<String> = keys
        .iter()
        .map(|key| key.replace(char::is_whitespace, "_"))
        .collect();
    let mut out = format!(
        "#EventTableVersion: {VERSION}\n#ListTitle: {}\n#FieldNames: start_time stop_time {}\n#FieldUnits: DateTime DateTime {}\n#FieldNulls: 0001-01-01T00:00:00.000Z 0001-01-01T00:00:00.000Z {}\n",
        title.replace('\n', " "),
        names.join(" "),
        keys.iter()
            .map(|key| unit(key))
            .collect::<Vec<_>>()
            .join(" "),
        vec!["NULL"; keys.len()].join(" "),
    );
    for member in members {
        let (start, stop, attributes) = match member {
            Member::Event { record, .. } => (record.start, record.stop, Some(&record.attributes)),
            Member::Derived { start, stop, .. } => (*start, *stop, None),
        };
        let mut row = vec![start.to_string(), stop.to_string()];
        row.extend(
            keys.iter()
                .map(|key| written_cell(attributes.and_then(|attributes| attributes.get(*key)))),
        );
        out.push_str(&row.join("\t"));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests;
