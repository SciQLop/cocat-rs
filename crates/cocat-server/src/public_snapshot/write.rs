//! The files of a snapshot, written from each public catalogue's JSON-lines file
//! (`public_search`). Only the fields listed here leave the server: no author, no creation
//! time, no origin, no products, no shape.
//!
//! Each table is written twice, from the same batches: as Parquet, which Python and most
//! tools read, and as Arrow IPC, which the browser's DuckDB reads without any extension
//! (its builds load Parquet support from the network, which the page's sandbox refuses).
//! The Arrow files are not compressed: that DuckDB cannot read compressed ones. HTTP
//! compression shrinks them on the wire, and the repeated text columns are dictionaries.

use std::io::BufRead as _;
use std::path::Path;
use std::sync::Arc;

use arrow_array::builder::{ListBuilder, StringBuilder};
use arrow_array::types::{Int8Type, Int32Type};
use arrow_array::{
    ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray,
    TimestampMicrosecondArray,
};
use arrow_array::{DictionaryArray, Int8Array, Int32Array};
use arrow_ipc::writer::FileWriter;
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
    let ids: Vec<&str> = catalogues
        .iter()
        .map(|catalogue| catalogue.id.as_str())
        .collect();
    let mut sink = EventSink::new(&ids)?;
    let mut files = Vec::new();
    let mut rows = Vec::new();
    for (catalogue, path) in catalogues.iter().zip(events) {
        let records = read_events(path)?;
        let first = sink.next;
        sink.push(&records)?;
        let attributes = attribute_batch(first, &records)?
            .map(|batch| {
                written(
                    &format!("attributes/{}", catalogue.id),
                    &catalogue.id,
                    &batch,
                )
            })
            .transpose()?
            .unwrap_or_default();
        let keys = attribute_keys(&records);
        let file_name = attributes.first().map(named);
        files.extend(attributes);
        rows.push((records.len() as i64, keys, file_name));
    }
    let (parquet, arrow) = sink.finish()?;
    files.push(Written {
        logical: "events.parquet".into(),
        stem: "events".into(),
        bytes: parquet,
    });
    files.push(Written {
        logical: "events.arrow".into(),
        stem: "events".into(),
        bytes: arrow,
    });
    files.extend(written(
        "catalogues",
        "catalogues",
        &catalogue_batch(catalogues, &rows)?,
    )?);
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

/// `batch` as `<logical>.parquet` and `<logical>.arrow`, Parquet first.
fn written(logical: &str, stem: &str, batch: &RecordBatch) -> Result<Vec<Written>, WriteError> {
    let mut parquet = ArrowWriter::try_new(Vec::new(), batch.schema(), Some(properties()))?;
    parquet.write(batch)?;
    let mut arrow = FileWriter::try_new(Vec::new(), &batch.schema())?;
    arrow.write(batch)?;
    arrow.finish()?;
    Ok(vec![
        Written {
            logical: format!("{logical}.parquet"),
            stem: stem.to_owned(),
            bytes: parquet.into_inner()?,
        },
        Written {
            logical: format!("{logical}.arrow"),
            stem: stem.to_owned(),
            bytes: arrow.into_inner()?,
        },
    ])
}

fn micros(at: cocat_core::time::Timestamp) -> i64 {
    i64::try_from(at.unix_micros()).unwrap_or(i64::MAX)
}

fn timestamp() -> DataType {
    DataType::Timestamp(TimeUnit::Microsecond, None)
}

/// An event's status, in the order of the dictionary every batch shares.
const STATUSES: [&str; 6] = [
    "preliminary",
    "confirmed",
    "reviewed",
    "final",
    "rejected",
    "deleted",
];

/// The events files, written one catalogue at a time so a row group is all that is held.
/// The catalogue and status columns are dictionaries with the same values in every batch:
/// an Arrow file allows one dictionary per column.
struct EventSink {
    parquet: ArrowWriter<Vec<u8>>,
    arrow: FileWriter<Vec<u8>>,
    schema: Arc<Schema>,
    catalogues: ArrayRef,
    statuses: ArrayRef,
    at: i32,
    next: i64,
}

impl EventSink {
    fn new(catalogues: &[&str]) -> Result<Self, WriteError> {
        let tags = DataType::List(Arc::new(Field::new("item", DataType::Utf8, true)));
        let text = |keys: DataType| DataType::Dictionary(Box::new(keys), Box::new(DataType::Utf8));
        let schema = Arc::new(Schema::new(vec![
            Field::new("event", DataType::Int64, false),
            Field::new("catalogue", text(DataType::Int32), false),
            Field::new("start", timestamp(), false),
            Field::new("stop", timestamp(), false),
            Field::new("status", text(DataType::Int8), true),
            Field::new("confidence", DataType::Float64, true),
            Field::new("start_err", DataType::Float64, true),
            Field::new("stop_err", DataType::Float64, true),
            Field::new("tags", tags, false),
            Field::new("observer_raw", DataType::Utf8, true),
        ]));
        let parquet = ArrowWriter::try_new(Vec::new(), Arc::clone(&schema), Some(properties()))?;
        let arrow = FileWriter::try_new(Vec::new(), &schema)?;
        Ok(Self {
            parquet,
            arrow,
            schema,
            catalogues: Arc::new(StringArray::from_iter_values(catalogues)),
            statuses: Arc::new(StringArray::from_iter_values(STATUSES)),
            at: 0,
            next: 1,
        })
    }

    /// The events of the next catalogue, in the order `new` named them.
    fn push(&mut self, events: &[EventRecord]) -> Result<(), WriteError> {
        let catalogue = self.at;
        self.at += 1;
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
            Arc::new(DictionaryArray::<Int32Type>::try_new(
                Int32Array::from(vec![catalogue; events.len()]),
                Arc::clone(&self.catalogues),
            )?),
            Arc::new(TimestampMicrosecondArray::from_iter_values(
                events.iter().map(|event| micros(event.start)),
            )),
            Arc::new(TimestampMicrosecondArray::from_iter_values(
                events.iter().map(|event| micros(event.stop)),
            )),
            Arc::new(DictionaryArray::<Int8Type>::try_new(
                Int8Array::from_iter(events.iter().map(status)),
                Arc::clone(&self.statuses),
            )?),
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
        self.parquet.write(&batch)?;
        self.arrow.write(&batch)?;
        Ok(())
    }

    /// The Parquet file and the Arrow file.
    fn finish(mut self) -> Result<(Vec<u8>, Vec<u8>), WriteError> {
        self.arrow.finish()?;
        Ok((self.parquet.into_inner()?, self.arrow.into_inner()?))
    }
}

/// The status's place in `STATUSES`.
fn status(event: &EventRecord) -> Option<i8> {
    let Ok(Value::String(status)) = serde_json::to_value(event.status) else {
        return None;
    };
    STATUSES
        .iter()
        .position(|known| *known == status)
        .and_then(|at| i8::try_from(at).ok())
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

/// The attributes of one catalogue: a row per event that has attributes, a typed column per
/// key. `None` when no event has any.
fn attribute_batch(first: i64, events: &[EventRecord]) -> Result<Option<RecordBatch>, WriteError> {
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
    Ok(Some(RecordBatch::try_new(
        Arc::new(Schema::new(fields)),
        columns,
    )?))
}

fn catalogue_batch(
    catalogues: &[Catalogue],
    rows: &[(i64, Vec<String>, Option<String>)],
) -> Result<RecordBatch, WriteError> {
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
    Ok(RecordBatch::try_new(Arc::new(schema), columns)?)
}
