//! The Parquet files of a snapshot, written from each public catalogue's JSON-lines file
//! (`public_search`). Only the fields listed here leave the server: no author, no creation
//! time, no origin, no products, no shape.

use std::io::BufRead as _;
use std::path::Path;
use std::sync::Arc;

use arrow_array::builder::{ListBuilder, StringBuilder};
use arrow_array::{
    ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray,
    TimestampMicrosecondArray,
};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use cocat_core::model::EventRecord;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use serde_json::Value;

/// The attributes the survey reads an event's observer from, first found wins.
const OBSERVER_KEYS: [&str; 3] = ["spacecraft", "stereo", "location"];
const ROW_GROUP: usize = 128 * 1024;
/// The columns every attribute file starts with; an attribute of the same name gets a `_`.
const EVENT_KEY: &str = "_event";
const UUID_KEY: &str = "_uuid";

/// One public catalogue as the snapshot describes it.
pub struct Catalogue {
    pub id: String,
    pub name: String,
    pub folder: Option<String>,
    pub source: Option<String>,
    pub description: Option<String>,
    pub relay_source: Option<String>,
    pub relay_url: Option<String>,
    pub relay_published: Option<bool>,
}

/// A written file's logical name (`events.parquet`, `attributes/<id>.parquet`), its stem for
/// the hashed name, and its bytes.
pub struct Written {
    pub logical: String,
    pub stem: String,
    pub bytes: Vec<u8>,
}

/// Every file of a snapshot; `events` holds each catalogue's JSON-lines path, in the same
/// order as `catalogues`.
pub fn snapshot(
    catalogues: &[Catalogue],
    events: &[&Path],
    named: &dyn Fn(&Written) -> String,
) -> Result<Vec<Written>, WriteError> {
    let mut sink = EventSink::new()?;
    let mut files = Vec::new();
    let mut rows = Vec::new();
    for (catalogue, path) in catalogues.iter().zip(events) {
        let records = read_events(path)?;
        let first = sink.next;
        sink.push(&catalogue.id, &records)?;
        let attributes = attribute_file(&catalogue.id, first, &records)?;
        let keys = attribute_keys(&records);
        let file_name = attributes.as_ref().map(named);
        files.extend(attributes);
        rows.push((records.len() as i64, keys, file_name));
    }
    files.push(Written {
        logical: "events.parquet".into(),
        stem: "events".into(),
        bytes: sink.finish()?,
    });
    files.push(Written {
        logical: "catalogues.parquet".into(),
        stem: "catalogues".into(),
        bytes: catalogue_file(catalogues, &rows)?,
    });
    Ok(files)
}

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Arrow(#[from] arrow_schema::ArrowError),
    #[error(transparent)]
    Parquet(#[from] parquet::errors::ParquetError),
}

/// The live events of a JSON-lines snapshot, by start, stop and uuid. Its first line is the
/// header.
fn read_events(path: &Path) -> Result<Vec<EventRecord>, WriteError> {
    let file = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut events = Vec::new();
    for line in file.lines().skip(1) {
        events.push(serde_json::from_str::<EventRecord>(&line?)?);
    }
    events.sort_by(|a, b| {
        (a.start.unix_micros(), a.stop.unix_micros(), &a.uuid).cmp(&(
            b.start.unix_micros(),
            b.stop.unix_micros(),
            &b.uuid,
        ))
    });
    Ok(events)
}

fn properties() -> WriterProperties {
    WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::default()))
        .set_max_row_group_row_count(Some(ROW_GROUP))
        .build()
}

fn to_parquet(batch: &RecordBatch) -> Result<Vec<u8>, WriteError> {
    let mut bytes = Vec::new();
    let mut writer = ArrowWriter::try_new(&mut bytes, batch.schema(), Some(properties()))?;
    writer.write(batch)?;
    writer.close()?;
    Ok(bytes)
}

fn micros(at: cocat_core::time::Timestamp) -> i64 {
    i64::try_from(at.unix_micros()).unwrap_or(i64::MAX)
}

fn timestamp() -> DataType {
    DataType::Timestamp(TimeUnit::Microsecond, None)
}

/// The events file, written one catalogue at a time so a row group is all that is held.
struct EventSink {
    writer: ArrowWriter<Vec<u8>>,
    schema: Arc<Schema>,
    next: i64,
}

impl EventSink {
    fn new() -> Result<Self, WriteError> {
        let tags = DataType::List(Arc::new(Field::new("item", DataType::Utf8, true)));
        let schema = Arc::new(Schema::new(vec![
            Field::new("event", DataType::Int64, false),
            Field::new("catalogue", DataType::Utf8, false),
            Field::new("start", timestamp(), false),
            Field::new("stop", timestamp(), false),
            Field::new("status", DataType::Utf8, false),
            Field::new("confidence", DataType::Float64, true),
            Field::new("start_err", DataType::Float64, true),
            Field::new("stop_err", DataType::Float64, true),
            Field::new("tags", tags, false),
            Field::new("observer_raw", DataType::Utf8, true),
        ]));
        let writer = ArrowWriter::try_new(Vec::new(), Arc::clone(&schema), Some(properties()))?;
        Ok(Self {
            writer,
            schema,
            next: 1,
        })
    }

    fn push(&mut self, catalogue: &str, events: &[EventRecord]) -> Result<(), WriteError> {
        if events.is_empty() {
            return Ok(());
        }
        let numbers: Vec<i64> = (self.next..).take(events.len()).collect();
        self.next += events.len() as i64;
        let observer = observer_key(events);
        let mut tags = ListBuilder::new(StringBuilder::new());
        for event in events {
            for tag in &event.tags {
                tags.values().append_value(tag);
            }
            tags.append(true);
        }
        let columns: Vec<ArrayRef> = vec![
            Arc::new(Int64Array::from(numbers)),
            Arc::new(StringArray::from(vec![catalogue; events.len()])),
            Arc::new(TimestampMicrosecondArray::from_iter_values(
                events.iter().map(|event| micros(event.start)),
            )),
            Arc::new(TimestampMicrosecondArray::from_iter_values(
                events.iter().map(|event| micros(event.stop)),
            )),
            Arc::new(StringArray::from_iter_values(events.iter().map(status))),
            Arc::new(Float64Array::from_iter(events.iter().map(|e| e.confidence))),
            Arc::new(Float64Array::from_iter(events.iter().map(|e| e.start_err))),
            Arc::new(Float64Array::from_iter(events.iter().map(|e| e.stop_err))),
            Arc::new(tags.finish()),
            Arc::new(StringArray::from_iter(events.iter().map(|event| {
                observer
                    .and_then(|key| event.attributes.get(key))
                    .and_then(as_text)
            }))),
        ];
        let batch = RecordBatch::try_new(Arc::clone(&self.schema), columns)?;
        self.writer.write(&batch)?;
        Ok(())
    }

    fn finish(self) -> Result<Vec<u8>, WriteError> {
        Ok(self.writer.into_inner()?)
    }
}

fn status(event: &EventRecord) -> String {
    match serde_json::to_value(event.status) {
        Ok(Value::String(status)) => status,
        _ => String::new(),
    }
}

/// The catalogue's observer attribute: the first of `OBSERVER_KEYS` an event carries.
fn observer_key(events: &[EventRecord]) -> Option<&'static str> {
    OBSERVER_KEYS.into_iter().find(|key| {
        events
            .iter()
            .any(|event| event.attributes.contains_key(*key))
    })
}

/// A value as text: a string as it is, anything else as JSON; `null` as nothing.
fn as_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

fn attribute_keys<'a>(events: impl IntoIterator<Item = &'a EventRecord>) -> Vec<String> {
    let mut keys = std::collections::BTreeSet::new();
    for event in events {
        keys.extend(event.attributes.keys().cloned());
    }
    keys.into_iter().collect()
}

/// What one attribute column holds. Fixed rules, so a type cannot drift with the data: any
/// number is a double, and a column mixing kinds is text.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Number,
    Boolean,
    Text,
}

fn kind_of(values: &[Option<&Value>]) -> Kind {
    let mut kinds = values
        .iter()
        .flatten()
        .filter(|value| !value.is_null())
        .map(|value| match value {
            Value::Number(_) => Kind::Number,
            Value::Bool(_) => Kind::Boolean,
            _ => Kind::Text,
        });
    let Some(first) = kinds.next() else {
        return Kind::Text;
    };
    if kinds.all(|kind| kind == first) {
        first
    } else {
        Kind::Text
    }
}

fn column(values: &[Option<&Value>]) -> (DataType, ArrayRef) {
    match kind_of(values) {
        Kind::Number => (
            DataType::Float64,
            Arc::new(Float64Array::from_iter(
                values.iter().map(|value| value.and_then(Value::as_f64)),
            )),
        ),
        Kind::Boolean => (
            DataType::Boolean,
            Arc::new(BooleanArray::from_iter(
                values.iter().map(|value| value.and_then(Value::as_bool)),
            )),
        ),
        Kind::Text => (
            DataType::Utf8,
            Arc::new(StringArray::from_iter(
                values.iter().map(|value| value.and_then(as_text)),
            )),
        ),
    }
}

/// The attribute file of one catalogue: a row per event that has attributes, a typed
/// column per key. `None` when no event has any.
fn attribute_file(
    catalogue: &str,
    first: i64,
    events: &[EventRecord],
) -> Result<Option<Written>, WriteError> {
    let carrying: Vec<(i64, &EventRecord)> = (first..)
        .zip(events)
        .filter(|(_, event)| !event.attributes.is_empty())
        .collect();
    if carrying.is_empty() {
        return Ok(None);
    }
    let mut fields = vec![
        Field::new(EVENT_KEY, DataType::Int64, false),
        Field::new(UUID_KEY, DataType::Utf8, false),
    ];
    let mut columns: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from_iter_values(
            carrying.iter().map(|(n, _)| *n),
        )),
        Arc::new(StringArray::from_iter_values(
            carrying.iter().map(|(_, event)| event.uuid.as_str()),
        )),
    ];
    let records: Vec<&EventRecord> = carrying.iter().map(|(_, event)| *event).collect();
    for key in attribute_keys(records.iter().copied()) {
        let values: Vec<Option<&Value>> = records
            .iter()
            .map(|event| event.attributes.get(&key))
            .collect();
        let (data_type, array) = column(&values);
        let name = if key == EVENT_KEY || key == UUID_KEY {
            format!("{key}_")
        } else {
            key
        };
        fields.push(Field::new(name, data_type, true));
        columns.push(array);
    }
    let batch = RecordBatch::try_new(Arc::new(Schema::new(fields)), columns)?;
    Ok(Some(Written {
        logical: format!("attributes/{catalogue}.parquet"),
        stem: catalogue.to_owned(),
        bytes: to_parquet(&batch)?,
    }))
}

fn catalogue_file(
    catalogues: &[Catalogue],
    rows: &[(i64, Vec<String>, Option<String>)],
) -> Result<Vec<u8>, WriteError> {
    let text = |pick: &dyn Fn(&Catalogue) -> Option<&str>| -> ArrayRef {
        Arc::new(StringArray::from_iter(catalogues.iter().map(pick)))
    };
    let keys = DataType::List(Arc::new(Field::new("item", DataType::Utf8, true)));
    let schema = Schema::new(vec![
        Field::new("catalogue", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("folder", DataType::Utf8, true),
        Field::new("source", DataType::Utf8, true),
        Field::new("description", DataType::Utf8, true),
        Field::new("relay_source", DataType::Utf8, true),
        Field::new("relay_url", DataType::Utf8, true),
        Field::new("relay_published", DataType::Boolean, true),
        Field::new("events", DataType::Int64, false),
        Field::new("attributes", keys, false),
        Field::new("attributes_file", DataType::Utf8, true),
    ]);
    let mut attributes = ListBuilder::new(StringBuilder::new());
    for (_, keys, _) in rows {
        for key in keys {
            attributes.values().append_value(key);
        }
        attributes.append(true);
    }
    let columns: Vec<ArrayRef> = vec![
        text(&|c| Some(c.id.as_str())),
        text(&|c| Some(c.name.as_str())),
        text(&|c| c.folder.as_deref()),
        text(&|c| c.source.as_deref()),
        text(&|c| c.description.as_deref()),
        text(&|c| c.relay_source.as_deref()),
        text(&|c| c.relay_url.as_deref()),
        Arc::new(BooleanArray::from_iter(
            catalogues.iter().map(|c| c.relay_published),
        )),
        Arc::new(Int64Array::from_iter_values(rows.iter().map(|row| row.0))),
        Arc::new(attributes.finish()),
        Arc::new(StringArray::from_iter(
            rows.iter().map(|row| row.2.as_deref()),
        )),
    ];
    to_parquet(&RecordBatch::try_new(Arc::new(schema), columns)?)
}
