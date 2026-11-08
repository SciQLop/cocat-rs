//! A catalogue as a VOTable, the shape cocat's own importer reads back.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::io;

use quick_xml::escape::{resolve_predefined_entity, unescape};
use quick_xml::events::{BytesDecl, BytesEnd, BytesRef, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer, XmlVersion};
use serde_json::Value;

use yrs::TransactionMut;

use crate::model::{
    self, CatalogueSummary, EventRecord, NewCatalogue, Status, WriteError, Written,
};
use crate::time::Timestamp;

#[derive(Debug, thiserror::Error)]
pub enum VotableError {
    #[error("Export VOTable: not all attributes are present in all events {0:?}")]
    MissingAttributes(Vec<String>),
    #[error("Export VOTable: not all value types are identical for all events for attribute {0}")]
    MixedTypes(String),
    #[error("Export VOTable: cannot write the document: {0}")]
    Encoding(#[from] io::Error),
    #[error("Import VOTable: the document is not readable: {0}")]
    Malformed(String),
    #[error("Import VOTable: cannot import field: {0}")]
    UnknownField(String),
    #[error("Import VOTable: required fields are missing for table {0}")]
    MissingTimes(String),
    #[error("Import VOTable: cannot read the value {1} of field {0}")]
    BadValue(String, String),
}

const START_FIELD: [(&str, &str); 6] = [
    ("ID", "TimeIntervalStart"),
    ("ucd", "time.start"),
    ("datatype", "char"),
    ("arraysize", "*"),
    ("xtype", "dateTime"),
    ("utype", ""),
];
const STOP_FIELD: [(&str, &str); 6] = [
    ("ID", "TimeIntervalStop"),
    ("ucd", "time.end"),
    ("datatype", "char"),
    ("arraysize", "*"),
    ("xtype", "dateTime"),
    ("utype", ""),
];
const CHAR: [(&str, &str); 2] = [("datatype", "char"), ("arraysize", "*")];
const DOUBLE: [(&str, &str); 1] = [("datatype", "double")];
const DATE: [(&str, &str); 3] = [
    ("datatype", "char"),
    ("arraysize", "*"),
    ("xtype", "dateTime"),
];

/// The event fields every table carries after its two times, in column order.
const FIXED: [(&str, &[(&str, &str)]); 10] = [
    ("author", &CHAR),
    ("uuid", &CHAR),
    ("tags", &CHAR),
    ("products", &CHAR),
    ("status", &CHAR),
    ("confidence", &DOUBLE),
    ("start_err", &DOUBLE),
    ("stop_err", &DOUBLE),
    ("source_id", &CHAR),
    ("created", &DATE),
];

pub fn export(
    catalogue: &CatalogueSummary,
    events: &[EventRecord],
) -> Result<String, VotableError> {
    export_many(&[(catalogue, events)])
}

pub fn export_many(
    catalogues: &[(&CatalogueSummary, &[EventRecord])],
) -> Result<String, VotableError> {
    let mut prepared = Vec::with_capacity(catalogues.len());
    for (catalogue, events) in catalogues {
        prepared.push(Prepared {
            catalogue,
            kinds: attribute_kinds(events)?,
            events: in_time_order(events),
        });
    }

    let mut writer = Writer::new(Vec::with_capacity(2048));
    writer.write_event(Event::Decl(BytesDecl::new("1.0", Some("utf-8"), None)))?;
    open(
        &mut writer,
        "VOTABLE",
        &[
            ("version", "1.4"),
            ("xmlns", "http://www.ivoa.net/xml/VOTable/v1.3"),
        ],
    )?;
    if let [one] = prepared.as_slice() {
        text(
            &mut writer,
            "DESCRIPTION",
            &format!(
                "Contact:{};Name:{}",
                one.catalogue.author.as_deref().unwrap_or_default(),
                one.catalogue.name
            ),
        )?;
    }
    open(&mut writer, "RESOURCE", &[])?;
    for table in &prepared {
        open(
            &mut writer,
            "TABLE",
            &[("name", &table.catalogue.name.replace(' ', "_"))],
        )?;
        write_fields(&mut writer, &table.kinds)?;
        open(&mut writer, "DATA", &[])?;
        open(&mut writer, "TABLEDATA", &[])?;
        for event in &table.events {
            write_row(&mut writer, event, &table.kinds)?;
        }
        close(&mut writer, "TABLEDATA")?;
        close(&mut writer, "DATA")?;
        close(&mut writer, "TABLE")?;
    }
    close(&mut writer, "RESOURCE")?;
    close(&mut writer, "VOTABLE")?;

    let bytes = writer.into_inner();
    String::from_utf8(bytes).map_err(|error| VotableError::Encoding(io::Error::other(error)))
}

/// One catalogue and the event table written for it.
struct Prepared<'a> {
    catalogue: &'a CatalogueSummary,
    kinds: Vec<(String, Kind)>,
    events: Vec<&'a EventRecord>,
}

/// Python writes the rows in catalogue order; ordering by start makes the export stable
/// whatever order the set-like membership gives us.
fn in_time_order(events: &[EventRecord]) -> Vec<&EventRecord> {
    let mut ordered: Vec<&EventRecord> = events.iter().collect();
    ordered.sort_by(|left, right| {
        (left.start, left.uuid.as_str()).cmp(&(right.start, right.uuid.as_str()))
    });
    ordered
}

fn open(writer: &mut Writer<Vec<u8>>, name: &str, attributes: &[(&str, &str)]) -> io::Result<()> {
    let mut element = BytesStart::new(name);
    for (key, value) in attributes {
        element.push_attribute((*key, sanitise(value).as_ref()));
    }
    writer.write_event(Event::Start(element))
}

fn close(writer: &mut Writer<Vec<u8>>, name: &str) -> io::Result<()> {
    writer.write_event(Event::End(BytesEnd::new(name)))
}

fn text(writer: &mut Writer<Vec<u8>>, name: &str, content: &str) -> io::Result<()> {
    open(writer, name, &[])?;
    writer.write_event(Event::Text(BytesText::new(&sanitise(content))))?;
    close(writer, name)
}

fn field(writer: &mut Writer<Vec<u8>>, name: &str, attributes: &[(&str, &str)]) -> io::Result<()> {
    let mut element = BytesStart::new("FIELD");
    element.push_attribute(("name", sanitise(name).as_ref()));
    for (key, value) in attributes {
        element.push_attribute((*key, *value));
    }
    writer.write_event(Event::Empty(element))
}

fn write_fields(writer: &mut Writer<Vec<u8>>, attributes: &[(String, Kind)]) -> io::Result<()> {
    field(writer, "Start Time", &START_FIELD)?;
    field(writer, "Stop Time", &STOP_FIELD)?;
    for (name, attributes) in FIXED {
        field(writer, name, attributes)?;
    }
    for (name, kind) in attributes {
        kind.write_field(writer, name)?;
    }
    Ok(())
}

fn write_row(
    writer: &mut Writer<Vec<u8>>,
    event: &EventRecord,
    attributes: &[(String, Kind)],
) -> io::Result<()> {
    let optional = |value: Option<String>| value.unwrap_or_default();
    let number = |value: Option<f64>| optional(value.map(|value| value.to_string()));
    let fixed = [
        event.start.to_string(),
        event.stop.to_string(),
        optional(event.author.clone()),
        event.uuid.clone(),
        json_list(&event.tags),
        json_list(&event.products),
        event.status.name().to_owned(),
        number(event.confidence),
        number(event.start_err),
        number(event.stop_err),
        optional(event.source_id.clone()),
        optional(event.created.map(|created| created.to_string())),
    ];
    let drawn: Vec<String> = attributes
        .iter()
        .map(|(name, kind)| kind.cell(&event.attributes[name]))
        .collect();
    open(writer, "TR", &[])?;
    for cell in fixed.iter().chain(&drawn) {
        text(writer, "TD", cell)?;
    }
    close(writer, "TR")
}

/// The attributes every event carries, by name, with the one kind they all share.
fn attribute_kinds(events: &[EventRecord]) -> Result<Vec<(String, Kind)>, VotableError> {
    let names = attribute_names(events);
    let mut common: Option<BTreeSet<&str>> = None;
    for event in events {
        let carried = keys(&event.attributes);
        common = Some(match common {
            None => carried,
            Some(seen) => seen.intersection(&carried).copied().collect(),
        });
    }
    let common = common.unwrap_or_default();

    let missing: Vec<String> = names
        .difference(&common)
        .map(|name| (*name).to_owned())
        .collect();
    if !missing.is_empty() {
        return Err(VotableError::MissingAttributes(missing));
    }

    let mut kinds = Vec::with_capacity(names.len());
    for name in &names {
        let mut kind: Option<Kind> = None;
        for event in events {
            let Some(value) = Kind::of(&event.attributes[*name]) else {
                return Err(VotableError::MixedTypes((*name).to_owned()));
            };
            match kind {
                None => kind = Some(value),
                Some(known) if known == value => {}
                Some(_) => return Err(VotableError::MixedTypes((*name).to_owned())),
            }
        }
        if let Some(kind) = kind {
            kinds.push(((*name).to_owned(), kind));
        }
    }
    Ok(kinds)
}

fn attribute_names(events: &[EventRecord]) -> BTreeSet<&str> {
    events
        .iter()
        .flat_map(|event| keys(&event.attributes))
        .collect()
}

fn keys(attributes: &serde_json::Map<String, Value>) -> BTreeSet<&str> {
    attributes.keys().map(String::as_str).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    /// Every JSON number, integer or not, is one `double` column, as astropy widens an
    /// integer column that holds a float.
    Number,
    Boolean,
    List,
}

impl Kind {
    fn of(value: &Value) -> Option<Self> {
        match value {
            Value::String(_) => Some(Self::Text),
            Value::Bool(_) => Some(Self::Boolean),
            Value::Array(_) => Some(Self::List),
            Value::Number(_) => Some(Self::Number),
            Value::Null | Value::Object(_) => None,
        }
    }

    fn write_field(self, writer: &mut Writer<Vec<u8>>, name: &str) -> io::Result<()> {
        match self {
            Self::Text => field(writer, name, &CHAR),
            Self::List => field(
                writer,
                name,
                &[("datatype", "char"), ("arraysize", "*"), ("utype", "json")],
            ),
            Self::Number => field(writer, name, &[("datatype", "double")]),
            Self::Boolean => field(writer, name, &[("datatype", "boolean")]),
        }
    }

    fn cell(self, value: &Value) -> String {
        match (self, value) {
            (Self::Text, Value::String(text)) => text.clone(),
            _ => value.to_string(),
        }
    }
}

fn json_list(items: &[String]) -> String {
    let values: Vec<Value> = items
        .iter()
        .map(|item| Value::from(item.as_str()))
        .collect();
    Value::Array(values).to_string()
}

/// XML 1.0 has no way to represent a control character, not even as a character reference,
/// so the data is cleaned before the writer escapes it.
fn sanitise(text: &str) -> Cow<'_, str> {
    let unrepresentable =
        |character: char| character < ' ' && !matches!(character, '\t' | '\n' | '\r');
    if !text.chars().any(unrepresentable) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .map(|character| {
                if unrepresentable(character) {
                    '\u{FFFD}'
                } else {
                    character
                }
            })
            .collect(),
    )
}

/// A VOTable read back into the parts an importer needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Imported {
    /// The `Contact` of the description, when the file names one.
    pub author: Option<String>,
    /// The `Name` of the description, when the file names one.
    pub name: Option<String>,
    pub tables: Vec<ImportedTable>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportedTable {
    /// The `TABLE` name, as written.
    pub name: String,
    pub events: Vec<EventRecord>,
}

const START: &str = "Start Time";
const STOP: &str = "Stop Time";

fn is_standard(name: &str) -> bool {
    name == START || name == STOP || FIXED.iter().any(|(fixed, _)| *fixed == name)
}

const DEFAULT_AUTHOR: &str = "VOTable Import";

pub fn import(xml: &str) -> Result<Imported, VotableError> {
    let (description, tables) = read(xml)?;
    let (author, name) = description_parts(&description);
    let mut imported = Vec::with_capacity(tables.len());
    for table in tables {
        imported.push(ImportedTable {
            name: table.name.clone(),
            events: events(&table)?,
        });
    }
    Ok(Imported {
        author,
        name,
        tables: imported,
    })
}

/// Writes each table as a new catalogue holding its events, in the caller's transaction. An
/// event whose uuid the project holds is replaced.
pub fn write(
    txn: &mut TransactionMut<'_>,
    imported: &Imported,
    name: Option<&str>,
) -> Result<Written, WriteError> {
    let author = imported.author.as_deref().unwrap_or(DEFAULT_AUTHOR);
    let mut written = Written::default();
    for (index, table) in imported.tables.iter().enumerate() {
        for event in &table.events {
            written.warnings.extend(model::create_event(txn, event)?);
        }
        let catalogue = NewCatalogue {
            uuid: uuid::Uuid::new_v4().to_string(),
            name: catalogue_name(imported, name, index),
            author: Some(author.to_owned()),
            ..NewCatalogue::default()
        };
        model::create_catalogue(txn, &catalogue)?;
        let uuids: Vec<String> = table
            .events
            .iter()
            .map(|event| event.uuid.clone())
            .collect();
        model::add_catalogue_events(txn, &catalogue.uuid, &uuids);
        written.catalogues.push(catalogue.uuid);
    }
    Ok(written)
}

/// The name asked for (with `_0`, `_1`… when there are several tables), else the file's
/// name for its one table, else the table's own name.
fn catalogue_name(imported: &Imported, name: Option<&str>, index: usize) -> String {
    let single = imported.tables.len() == 1;
    let table = imported.tables[index].name.as_str();
    let named = |base: &str| match single {
        true => base.to_owned(),
        false => format!("{base}_{index}"),
    };
    match (name, imported.name.as_deref().filter(|_| single)) {
        (Some(name), _) | (None, Some(name)) => named(name),
        (None, None) if !table.is_empty() => table.to_owned(),
        (None, None) => named("Imported Catalogue"),
    }
}

struct Table {
    name: String,
    fields: Vec<Field>,
    rows: Vec<Vec<String>>,
}

impl Table {
    fn new(name: String) -> Self {
        Self {
            name,
            fields: Vec::new(),
            rows: Vec::new(),
        }
    }
}

struct Field {
    name: String,
    datatype: String,
    utype: String,
}

fn read(xml: &str) -> Result<(String, Vec<Table>), VotableError> {
    let mut reader = Reader::from_str(xml);
    let mut description = String::new();
    let mut in_description = false;
    let mut cell: Option<String> = None;
    let mut row: Option<Vec<String>> = None;
    let mut table: Option<Table> = None;
    let mut tables = Vec::new();

    loop {
        match reader.read_event().map_err(malformed)? {
            Event::Start(element) => match element.name().as_ref() {
                "DESCRIPTION" => in_description = true,
                "TABLE" => {
                    table = Some(Table::new(attribute(&element, "name").unwrap_or_default()));
                }
                "FIELD" => {
                    if let Some(table) = table.as_mut() {
                        table.fields.push(read_field(&element));
                    }
                }
                "TR" => row = Some(Vec::new()),
                "TD" => cell = Some(String::new()),
                _ => {}
            },
            Event::Empty(element) => {
                if element.name().as_ref() == "FIELD"
                    && let Some(table) = table.as_mut()
                {
                    table.fields.push(read_field(&element));
                }
            }
            Event::Text(text) => {
                push_text(
                    &mut description,
                    &mut cell,
                    in_description,
                    &text_content(&text)?,
                );
            }
            Event::GeneralRef(reference) => {
                push_text(
                    &mut description,
                    &mut cell,
                    in_description,
                    &reference_text(&reference),
                );
            }
            Event::End(element) => match element.name().as_ref() {
                "DESCRIPTION" => in_description = false,
                "TD" => {
                    if let (Some(done), Some(row)) = (cell.take(), row.as_mut()) {
                        row.push(done.trim().to_owned());
                    }
                }
                "TR" => {
                    if let (Some(done), Some(table)) = (row.take(), table.as_mut()) {
                        table.rows.push(done);
                    }
                }
                "TABLE" => {
                    if let Some(done) = table.take() {
                        tables.push(done);
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }

    Ok((description, tables))
}

fn push_text(
    description: &mut String,
    cell: &mut Option<String>,
    in_description: bool,
    text: &str,
) {
    if in_description {
        description.push_str(text);
    }
    if let Some(cell) = cell.as_mut() {
        cell.push_str(text);
    }
}

/// The reader reports `&quot;` and friends as their own event, so the text of a cell is the
/// concatenation of the text events and of the references between them.
fn reference_text(reference: &BytesRef<'_>) -> String {
    if let Some(resolved) = resolve_predefined_entity(reference.as_ref()) {
        return resolved.to_owned();
    }
    reference
        .resolve_char_ref()
        .ok()
        .flatten()
        .map(String::from)
        .unwrap_or_default()
}

fn malformed(error: impl std::fmt::Display) -> VotableError {
    VotableError::Malformed(error.to_string())
}

fn text_content(text: &BytesText<'_>) -> Result<String, VotableError> {
    unescape(text.as_ref())
        .map(Cow::into_owned)
        .map_err(malformed)
}

fn attribute(element: &BytesStart<'_>, key: &str) -> Option<String> {
    element
        .attributes()
        .flatten()
        .find(|attribute| attribute.key.as_ref() == key)
        .and_then(|attribute| attribute.normalized_value(XmlVersion::Implicit1_0).ok())
        .map(Cow::into_owned)
}

fn read_field(element: &BytesStart<'_>) -> Field {
    Field {
        name: attribute(element, "name").unwrap_or_default(),
        datatype: attribute(element, "datatype").unwrap_or_default(),
        utype: attribute(element, "utype").unwrap_or_default(),
    }
}

fn description_parts(description: &str) -> (Option<String>, Option<String>) {
    let mut author = None;
    let mut name = None;
    for part in description.split(';') {
        if let Some((key, value)) = part.split_once(':') {
            match key.trim() {
                "Contact" => author = Some(value.trim().to_owned()),
                "Name" => name = Some(value.trim().to_owned()),
                _ => {}
            }
        }
    }
    (author, name)
}

fn events(table: &Table) -> Result<Vec<EventRecord>, VotableError> {
    let named = |wanted: &str| table.fields.iter().any(|field| field.name == wanted);
    if !named(START) || !named(STOP) {
        return Err(VotableError::MissingTimes(table.name.clone()));
    }
    for field in &table.fields {
        if !is_standard(&field.name) && !Kind::importable(field) {
            return Err(VotableError::UnknownField(field.name.clone()));
        }
    }

    let mut imported = Vec::with_capacity(table.rows.len());
    for row in &table.rows {
        imported.push(event(table, row)?);
    }
    Ok(imported)
}

fn event(table: &Table, row: &[String]) -> Result<EventRecord, VotableError> {
    let cell = |name: &str| {
        table
            .fields
            .iter()
            .zip(row)
            .find(|(field, _)| field.name == name)
            .map(|(_, value)| value.as_str())
            .filter(|value| !value.is_empty())
    };
    let time = |name: &str| {
        cell(name)
            .and_then(Timestamp::parse)
            .ok_or_else(|| VotableError::BadValue(name.to_owned(), String::new()))
    };
    let mut event = EventRecord::new(
        cell("uuid").map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_owned),
        time(START)?,
        time(STOP)?,
    );
    event.author = Some(cell("author").unwrap_or(DEFAULT_AUTHOR).to_owned());
    event.source_id = cell("source_id").map(str::to_owned);
    event.status = optional(cell("status"), "status", Status::from_name)?.unwrap_or_default();
    event.confidence = optional(cell("confidence"), "confidence", |v| v.parse().ok())?;
    event.start_err = optional(cell("start_err"), "start_err", |v| v.parse().ok())?;
    event.stop_err = optional(cell("stop_err"), "stop_err", |v| v.parse().ok())?;
    event.created = optional(cell("created"), "created", Timestamp::parse)?;

    for (field, value) in table.fields.iter().zip(row) {
        match field.name.as_str() {
            "tags" => event.tags = strings(field, value)?,
            "products" => event.products = strings(field, value)?,
            name if is_standard(name) => {}
            _ => {
                event
                    .attributes
                    .insert(field.name.clone(), Kind::import(field, value)?);
            }
        }
    }
    Ok(event)
}

/// A cell that may be empty, read with `parse`; a cell `parse` refuses is an error.
fn optional<T>(
    cell: Option<&str>,
    name: &str,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<Option<T>, VotableError> {
    cell.map(|value| {
        parse(value).ok_or_else(|| VotableError::BadValue(name.to_owned(), value.to_owned()))
    })
    .transpose()
}

fn strings(field: &Field, value: &str) -> Result<Vec<String>, VotableError> {
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str::<Vec<String>>(value)
        .map_err(|_| VotableError::BadValue(field.name.clone(), value.to_owned()))
}

impl Kind {
    /// Whether a field that is not one of cocat's own can be carried as an attribute.
    fn importable(field: &Field) -> bool {
        matches!(
            field.datatype.as_str(),
            "long" | "int" | "short" | "double" | "float" | "boolean" | "char"
        )
    }

    fn import(field: &Field, value: &str) -> Result<Value, VotableError> {
        let bad = || VotableError::BadValue(field.name.clone(), value.to_owned());
        let parsed = match field.datatype.as_str() {
            "long" | "int" | "short" => {
                Value::from(value.trim().parse::<i64>().map_err(|_| bad())?)
            }
            "double" | "float" => {
                let number: f64 = value.trim().parse().map_err(|_| bad())?;
                serde_json::Number::from_f64(number)
                    .map(Value::Number)
                    .ok_or_else(bad)?
            }
            "boolean" => Value::from(matches!(value.trim(), "true" | "1" | "T")),
            _ if field.utype == "json" => serde_json::from_str(value).map_err(|_| bad())?,
            _ => Value::from(value.to_owned()),
        };
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value, json};

    use super::{export, export_many, import, write};
    use crate::model::{CatalogueSummary, EventRecord, Status};
    use crate::time::Timestamp;
    use crate::votable::VotableError;

    fn catalogue(name: &str) -> CatalogueSummary {
        CatalogueSummary {
            uuid: "6f9c1a52-0b0d-4f4e-9d5e-1b2c3d4e5f60".to_owned(),
            name: name.to_owned(),
            author: Some("a@x.org".to_owned()),
            created: None,
            deleted: false,
            tags: vec!["t1".to_owned()],
            columns: Vec::new(),
            attributes: Map::new(),
            rule: None,
            event_count: 0,
        }
    }

    fn event(start: &str, confidence: Option<f64>) -> EventRecord {
        let start = Timestamp::parse(start).expect("a start");
        let mut event = EventRecord::new("00000000-0000-4000-8000-000000000001", start, start);
        event.author = Some("a@x.org".to_owned());
        event.tags = vec!["t1".to_owned()];
        event.products = vec!["p1".to_owned()];
        event.confidence = confidence;
        event
    }

    fn with_attributes(values: Value) -> EventRecord {
        let mut event = event("2025-01-31 10:00:00", Some(0.3));
        event.attributes = values.as_object().cloned().expect("an object");
        event
    }

    fn field(name: &str, datatype: &str, utype: &str) -> String {
        let utype = if utype.is_empty() {
            String::new()
        } else {
            format!(r#" utype="{utype}""#)
        };
        format!(r#"<FIELD name="{name}" datatype="{datatype}" arraysize="*"{utype}/>"#)
    }

    fn votable(fields: &[String], rows: &[&str]) -> String {
        format!(
            "<VOTABLE><RESOURCE><TABLE name=\"t\"><DATA><TABLEDATA>{}{}</TABLEDATA></DATA></TABLE></RESOURCE></VOTABLE>",
            fields.concat(),
            rows.concat()
        )
    }

    fn start_stop() -> Vec<String> {
        vec![
            field("Start Time", "char", ""),
            field("Stop Time", "char", ""),
        ]
    }

    #[test]
    fn an_exported_catalogue_imports_back() {
        let catalogue = catalogue("alpha");
        let first = with_attributes(json!({"q": 1.5, "mission": "MMS"}));
        let mut second = with_attributes(json!({"q": 2.5, "mission": "MMS"}));
        second.start = Timestamp::parse("2025-01-31 11:00:00").expect("a time");
        second.stop = second.start;
        second.confidence = None;
        second.status = Status::Rejected;
        second.start_err = Some(1.5);
        second.stop_err = Some(2.0);
        second.source_id = Some("list-7".to_owned());
        second.created = Some(Timestamp::parse("2025-02-01 00:00:00").expect("a time"));
        let xml = export(&catalogue, &[first.clone(), second.clone()]).expect("an export");

        let imported = import(&xml).expect("an import");

        assert_eq!(imported.author.as_deref(), Some("a@x.org"));
        assert_eq!(imported.name.as_deref(), Some("alpha"));
        assert_eq!(imported.tables.len(), 1);
        let table = &imported.tables[0];
        assert_eq!(table.name, "alpha");
        assert_eq!(table.events.len(), 2);
        assert_eq!(table.events[0].uuid, first.uuid);
        assert_eq!(table.events[0].start, first.start);
        assert_eq!(table.events[0].stop, first.stop);
        assert_eq!(table.events[0], first);
        assert_eq!(table.events[1], second);
    }

    #[test]
    fn export_many_writes_a_table_per_catalogue() {
        let first = catalogue("alpha");
        let second = catalogue("beta");
        let xml = export_many(&[(&first, &[]), (&second, &[])]).expect("an export");

        let imported = import(&xml).expect("an import");

        assert_eq!(imported.author, None);
        assert_eq!(imported.name, None);
        let names: Vec<&str> = imported
            .tables
            .iter()
            .map(|table| table.name.as_str())
            .collect();
        assert_eq!(names, ["alpha", "beta"]);
    }

    #[test]
    fn a_row_without_a_uuid_gets_a_fresh_one() {
        let rows = [
            "<TR><TD>2025-01-31 10:00:00+00:00</TD><TD>2025-01-31 10:01:00+00:00</TD></TR>",
            "<TR><TD>2025-01-31 11:00:00+00:00</TD><TD>2025-01-31 11:01:00+00:00</TD></TR>",
        ];
        let xml = votable(&start_stop(), &rows);

        let imported = import(&xml).expect("an import");

        let events = &imported.tables[0].events;
        assert_eq!(events.len(), 2);
        assert_ne!(events[0].uuid, events[1].uuid);
        assert_eq!(events[0].author.as_deref(), Some("VOTable Import"));
        assert_eq!(
            events[0].start,
            Timestamp::parse("2025-01-31 10:00:00").expect("a time")
        );
    }

    #[test]
    fn a_rating_column_is_an_attribute() {
        let mut fields = start_stop();
        fields.push(field("rating", "long", ""));
        let rows = ["<TR><TD>2025-01-31 10:00:00</TD><TD>2025-01-31 10:01:00</TD><TD>5</TD></TR>"];
        let xml = votable(&fields, &rows);

        let imported = import(&xml).expect("an import");

        let event = &imported.tables[0].events[0];
        assert_eq!(event.confidence, None);
        assert_eq!(event.attributes["rating"], json!(5));
    }

    #[test]
    fn an_unknown_status_is_refused() {
        let mut fields = start_stop();
        fields.push(field("status", "char", ""));
        let rows =
            ["<TR><TD>2025-01-31 10:00:00</TD><TD>2025-01-31 10:01:00</TD><TD>maybe</TD></TR>"];
        let error = import(&votable(&fields, &rows)).expect_err("a refusal");

        assert!(
            matches!(&error, VotableError::BadValue(name, value) if name == "status" && value == "maybe"),
            "{error:?}"
        );
    }

    #[test]
    fn an_attribute_takes_its_type_from_the_field() {
        let fields = vec![
            field("Start Time", "char", ""),
            field("Stop Time", "char", ""),
            field("count", "long", ""),
            field("q", "double", ""),
            field("flag", "boolean", ""),
            field("list", "char", "json"),
            field("mission", "char", ""),
        ];
        let rows = [
            "<TR><TD>2025-01-31 10:00:00</TD><TD>2025-01-31 10:01:00</TD><TD>2</TD><TD>1.5</TD><TD>true</TD><TD>[\"a\", \"b\"]</TD><TD>MMS</TD></TR>",
        ];
        let xml = votable(&fields, &rows);

        let imported = import(&xml).expect("an import");

        assert_eq!(
            imported.tables[0].events[0].attributes,
            json!({"count": 2, "q": 1.5, "flag": true, "list": ["a", "b"], "mission": "MMS"})
                .as_object()
                .cloned()
                .expect("an object")
        );
    }

    #[test]
    fn a_table_without_times_is_refused() {
        let xml = votable(&[field("author", "char", "")], &[]);

        let error = import(&xml).expect_err("a refusal");

        assert!(
            matches!(&error, VotableError::MissingTimes(name) if name == "t"),
            "{error:?}"
        );
    }

    #[test]
    fn an_unknown_field_is_refused() {
        let mut fields = start_stop();
        fields.push(field("temperature", "complex", ""));
        let xml = votable(&fields, &[]);

        let error = import(&xml).expect_err("a refusal");

        assert!(
            matches!(&error, VotableError::UnknownField(name) if name == "temperature"),
            "{error:?}"
        );
    }

    fn position(xml: &str, needle: &str) -> usize {
        xml.find(needle)
            .unwrap_or_else(|| panic!("{needle} is missing from\n{xml}"))
    }

    #[test]
    fn fixed_fields_in_python_order() {
        let xml = export(
            &catalogue("my room x"),
            &[event("2025-01-31 10:00:00", Some(0.3))],
        )
        .expect("a votable");

        assert!(xml.starts_with(r#"<?xml version="1.0" encoding="utf-8"?>"#));
        assert!(
            xml.contains(r#"<VOTABLE version="1.4" xmlns="http://www.ivoa.net/xml/VOTable/v1.3">"#)
        );
        assert!(xml.contains("<DESCRIPTION>Contact:a@x.org;Name:my room x</DESCRIPTION>"));
        assert!(xml.contains(r#"<TABLE name="my_room_x">"#));

        let order = [
            r#"<FIELD name="Start Time" ID="TimeIntervalStart" ucd="time.start" datatype="char" arraysize="*" xtype="dateTime" utype=""/>"#,
            r#"<FIELD name="Stop Time" ID="TimeIntervalStop" ucd="time.end" datatype="char" arraysize="*" xtype="dateTime" utype=""/>"#,
            r#"<FIELD name="author" datatype="char" arraysize="*"/>"#,
            r#"<FIELD name="uuid" datatype="char" arraysize="*"/>"#,
            r#"<FIELD name="tags" datatype="char" arraysize="*"/>"#,
            r#"<FIELD name="products" datatype="char" arraysize="*"/>"#,
            r#"<FIELD name="status" datatype="char" arraysize="*"/>"#,
            r#"<FIELD name="confidence" datatype="double"/>"#,
            r#"<FIELD name="start_err" datatype="double"/>"#,
            r#"<FIELD name="stop_err" datatype="double"/>"#,
            r#"<FIELD name="source_id" datatype="char" arraysize="*"/>"#,
            r#"<FIELD name="created" datatype="char" arraysize="*" xtype="dateTime"/>"#,
        ];
        let mut previous = 0;
        for field in order {
            let at = position(&xml, field);
            assert!(at > previous, "{field} is out of order in\n{xml}");
            previous = at;
        }
    }

    #[test]
    fn an_unset_field_is_an_empty_cell() {
        let xml = export(
            &catalogue("alpha"),
            &[
                event("2025-01-31 10:00:00", None),
                event("2025-01-31 11:00:00", Some(0.3)),
            ],
        )
        .expect("a votable");

        assert!(
            xml.contains("<TR><TD>2025-01-31T10:00:00.000000Z</TD>"),
            "{xml}"
        );
        assert!(xml.contains("<TD>confirmed</TD><TD></TD>"), "{xml}");
        assert!(xml.contains("<TD>confirmed</TD><TD>0.3</TD>"), "{xml}");
    }

    #[test]
    fn rows_are_in_time_order() {
        let late = event("2025-01-31 12:00:00", None);
        let early = event("2025-01-31 10:00:00", None);
        let xml = export(&catalogue("alpha"), &[late, early]).expect("a votable");

        assert!(
            position(&xml, "2025-01-31T10:00:00") < position(&xml, "2025-01-31T12:00:00"),
            "{xml}"
        );
    }

    #[test]
    fn attribute_types() {
        let event = with_attributes(json!({"s": "x", "i": 1, "f": 1.5, "b": true, "l": [1]}));
        let xml = export(&catalogue("alpha"), &[event]).expect("a votable");

        assert!(
            xml.contains(r#"<FIELD name="b" datatype="boolean"/>"#),
            "{xml}"
        );
        assert!(
            xml.contains(r#"<FIELD name="f" datatype="double"/>"#),
            "{xml}"
        );
        assert!(
            xml.contains(r#"<FIELD name="i" datatype="double"/>"#),
            "{xml}"
        );
        assert!(
            xml.contains(r#"<FIELD name="l" datatype="char" arraysize="*" utype="json"/>"#),
            "{xml}"
        );
        assert!(
            xml.contains(r#"<FIELD name="s" datatype="char" arraysize="*"/>"#),
            "{xml}"
        );
        assert!(
            xml.contains("<TD>true</TD><TD>1.5</TD><TD>1</TD><TD>[1]</TD><TD>x</TD>"),
            "{xml}"
        );
    }

    #[test]
    fn missing_attribute_is_an_error() {
        let mut without = event("2025-01-31 11:00:00", None);
        without.attributes = json!({"b": 2}).as_object().cloned().expect("an object");

        let error = export(
            &catalogue("alpha"),
            &[with_attributes(json!({"a": 1, "b": 2})), without],
        )
        .expect_err("a missing attribute");

        assert_eq!(
            error.to_string(),
            "Export VOTable: not all attributes are present in all events [\"a\"]"
        );
    }

    #[test]
    fn mixed_types_is_an_error() {
        let error = export(
            &catalogue("alpha"),
            &[
                with_attributes(json!({"a": 1})),
                with_attributes(json!({"a": "one"})),
            ],
        )
        .expect_err("a mixed type");

        assert_eq!(
            error.to_string(),
            "Export VOTable: not all value types are identical for all events for attribute a"
        );
    }

    #[test]
    fn escapes_markup() {
        let mut event = event("2025-01-31 10:00:00", None);
        event.attributes = json!({"note": "a<b & \"c\""})
            .as_object()
            .cloned()
            .expect("an object");
        let xml = export(&catalogue("a<b & \"c\""), &[event]).expect("a votable");

        assert!(
            xml.contains(
                "<DESCRIPTION>Contact:a@x.org;Name:a&lt;b &amp; &quot;c&quot;</DESCRIPTION>"
            ),
            "{xml}"
        );
        assert!(xml.contains("<TD>a&lt;b &amp; &quot;c&quot;</TD>"), "{xml}");
    }

    #[test]
    fn empty_catalogue_has_no_rows() {
        let xml = export(&catalogue("beta"), &[]).expect("a votable");

        assert!(
            xml.contains(r#"<FIELD name="confidence" datatype="double"/>"#),
            "{xml}"
        );
        assert!(xml.contains("<TABLEDATA>"), "{xml}");
        assert!(!xml.contains("<TR>"), "{xml}");
    }

    #[test]
    fn tag_with_markup_is_escaped() {
        let mut event = event("2025-01-31 10:00:00", None);
        event.tags = vec!["a<b".to_owned(), "x&y".to_owned()];
        let xml = export(&catalogue("alpha"), &[event]).expect("a votable");

        assert!(
            xml.contains(r#"[&quot;a&lt;b&quot;,&quot;x&amp;y&quot;]"#),
            "{xml}"
        );
        assert!(!xml.contains("a<b"), "{xml}");
    }

    #[test]
    fn list_attribute_with_markup_is_escaped() {
        let event = with_attributes(json!({"l": ["a<b", "x&y"]}));
        let xml = export(&catalogue("alpha"), &[event]).expect("a votable");

        assert!(
            xml.contains(r#"[&quot;a&lt;b&quot;,&quot;x&amp;y&quot;]"#),
            "{xml}"
        );
        assert!(!xml.contains("a<b"), "{xml}");
    }

    #[test]
    fn control_character_is_replaced() {
        let event = with_attributes(json!({"note": "a\u{1}b"}));
        let xml = export(&catalogue("alpha"), &[event]).expect("a votable");

        assert!(xml.contains("<TD>a\u{FFFD}b</TD>"), "{xml}");
        assert!(!xml.contains('\u{1}'), "{xml}");
    }

    #[test]
    fn control_character_in_a_name_is_replaced() {
        let event = with_attributes(json!({"n\u{1}te": "x"}));
        let xml = export(&catalogue("al\u{2}pha"), &[event]).expect("a votable");

        assert!(xml.contains("<TABLE name=\"al\u{FFFD}pha\">"), "{xml}");
        assert!(xml.contains("<FIELD name=\"n\u{FFFD}te\""), "{xml}");
        assert!(!xml.contains(['\u{1}', '\u{2}']), "{xml}");
    }

    #[test]
    fn carriage_return_is_written_as_a_reference() {
        let event = with_attributes(json!({"note": "a\rb"}));
        let xml = export(&catalogue("alpha"), &[event]).expect("a votable");

        assert!(xml.contains("<TD>a&#13;b</TD>"), "{xml}");
    }

    #[test]
    fn integer_and_float_attribute_is_one_double_field() {
        let mut whole = with_attributes(json!({"n": 2}));
        whole.uuid = "00000000-0000-4000-8000-000000000001".to_owned();
        let mut fractional = with_attributes(json!({"n": 2.5}));
        fractional.uuid = "00000000-0000-4000-8000-000000000002".to_owned();
        let xml = export(&catalogue("alpha"), &[whole, fractional]).expect("a votable");

        assert!(
            xml.contains(r#"<FIELD name="n" datatype="double"/>"#),
            "{xml}"
        );
        assert!(xml.contains("<TD>2</TD></TR>"), "{xml}");
        assert!(xml.contains("<TD>2.5</TD></TR>"), "{xml}");
    }

    #[test]
    fn each_table_is_written_as_a_new_catalogue() {
        let mut second = event("2025-01-31 11:00:00", None);
        second.uuid = "00000000-0000-4000-8000-000000000002".to_owned();
        let one = export(&catalogue("alpha"), &[event("2025-01-31 10:00:00", None)]).unwrap();
        let two = export_many(&[
            (&catalogue("alpha"), &[event("2025-01-31 10:00:00", None)]),
            (&catalogue("beta"), &[second]),
        ])
        .unwrap();
        let doc = yrs::Doc::new();
        let mut txn = yrs::Transact::transact_mut(&doc);

        let named = write(&mut txn, &import(&one).unwrap(), None).unwrap();
        let renamed = write(&mut txn, &import(&one).unwrap(), Some("mine")).unwrap();
        let many = write(&mut txn, &import(&two).unwrap(), None).unwrap();
        let mine = write(&mut txn, &import(&two).unwrap(), Some("mine")).unwrap();

        let name = |uuid: &str| crate::model::catalogue(&txn, uuid).unwrap().name;
        assert_eq!(name(&named.catalogues[0]), "alpha");
        assert_eq!(name(&renamed.catalogues[0]), "mine");
        assert_eq!(name(&many.catalogues[0]), "alpha");
        assert_eq!(name(&many.catalogues[1]), "beta");
        assert_eq!(name(&mine.catalogues[1]), "mine_1");
        assert_ne!(named.catalogues, renamed.catalogues);
        let listed = crate::model::listed_events(&txn, &many.catalogues[1]);
        assert_eq!(listed, ["00000000-0000-4000-8000-000000000002"]);
        let author = crate::model::catalogue(&txn, &named.catalogues[0])
            .unwrap()
            .author;
        assert_eq!(author.as_deref(), Some("a@x.org"));
    }
}
