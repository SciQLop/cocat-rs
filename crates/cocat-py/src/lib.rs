//! The compiled part of `cocat_rs`: a project, its catalogues and its events.
//!
//! Everything that can be done without Python types happens in Rust; the Python wrapper
//! (`cocat_rs/__init__.py`) adds the asyncio and listener plumbing that needs a loop.

mod server_errors;
mod sharing;
mod sign_in;
mod submissions;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use cocat_client::{Change, Credentials, Room as ClientRoom, RoomInfo as ClientRoomInfo};
use cocat_core::model::{
    self, AttributeType, EventField, EventRecord, Hop, Member, NewCatalogue, Origin, OriginKind,
    Status, Used, Vertex,
};
use cocat_core::time::Timestamp;
use cocat_core::{query, tfcat, tscat_json, votable};
use numpy::PyArray1;
use pyo3::exceptions::{PyLookupError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{
    PyDateAccess, PyDateTime, PyDict, PyList, PySet, PyTimeAccess, PyTuple, PyType, PyTzInfo,
};
use serde_json::{Map as JsonMap, Value};
use server_errors::http_error;
use yrs::{Map as _, ReadTxn as _, Transact, TransactionMut};

type Result<T> = PyResult<T>;

/// `save()` waits for the server's acknowledgement this long before it gives up.
const DEFAULT_SAVE_TIMEOUT: Duration = Duration::from_secs(30);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Every write goes through here, so a read-only project refuses them all alike.
fn write<T>(room: &ClientRoom, edit: impl FnOnce(&mut TransactionMut<'_>) -> T) -> Result<T> {
    room.write(edit)
        .map_err(|error| PyRuntimeError::new_err(error.to_string()))
}

/// A datetime as Python gets it: timezone-aware UTC, the form Python cocat returns.
fn to_python_datetime(py: Python<'_>, timestamp: Timestamp) -> Result<Py<PyAny>> {
    let parts = timestamp.parts();
    let utc = py.import("datetime")?.getattr("timezone")?.getattr("utc")?;
    let utc = utc.cast::<PyTzInfo>()?;
    Ok(PyDateTime::new(
        py,
        parts.year,
        parts.month,
        parts.day,
        parts.hour,
        parts.minute,
        parts.second,
        parts.microsecond,
        Some(utc),
    )?
    .into_any()
    .unbind())
}

/// A datetime argument: either a `datetime` or the text form a document holds. A naive value
/// is UTC, as Python cocat takes it.
fn to_timestamp(value: &Bound<'_, PyAny>) -> Result<Timestamp> {
    if let Ok(text) = value.extract::<String>() {
        return Timestamp::parse(&text)
            .ok_or_else(|| PyValueError::new_err(format!("not a timestamp: {text}")));
    }
    let datetime = value
        .cast::<PyDateTime>()
        .map_err(|_| PyValueError::new_err("expected a datetime or a timestamp string"))?;
    let aware = !datetime.call_method0("utcoffset")?.is_none();
    let datetime = if aware {
        let utc = value
            .py()
            .import("datetime")?
            .getattr("timezone")?
            .getattr("utc")?;
        datetime.call_method1("astimezone", (utc,))?
    } else {
        datetime.clone().into_any()
    };
    let datetime = datetime
        .cast::<PyDateTime>()
        .map_err(|_| PyValueError::new_err("the datetime could not be normalised to UTC"))?;
    let text = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:06}+00:00",
        datetime.get_year(),
        datetime.get_month(),
        datetime.get_day(),
        datetime.get_hour(),
        datetime.get_minute(),
        datetime.get_second(),
        datetime.get_microsecond(),
    );
    Timestamp::parse(&text).ok_or_else(|| PyValueError::new_err(format!("not a timestamp: {text}")))
}

fn to_json(value: &Bound<'_, PyAny>) -> Result<Value> {
    if value.is_none() {
        return Ok(Value::Null);
    }
    if value.cast::<PyDateTime>().is_ok() {
        return Ok(Value::from(to_timestamp(value)?.to_string()));
    }
    if let Ok(flag) = value.extract::<bool>() {
        return Ok(Value::Bool(flag));
    }
    if let Ok(integer) = value.extract::<i64>() {
        return Ok(Value::from(integer));
    }
    if let Ok(float) = value.extract::<f64>() {
        return Ok(Value::from(float));
    }
    if let Ok(text) = value.extract::<String>() {
        return Ok(Value::from(text));
    }
    if let Ok(items) = value.cast::<PyList>() {
        let mut list = Vec::with_capacity(items.len());
        for item in items.iter() {
            list.push(to_json(&item)?);
        }
        return Ok(Value::Array(list));
    }
    if let Ok(dict) = value.cast::<PyDict>() {
        let mut map = JsonMap::new();
        for (key, item) in dict.iter() {
            map.insert(key.extract::<String>()?, to_json(&item)?);
        }
        return Ok(Value::Object(map));
    }
    Err(PyValueError::new_err(
        "attributes hold only null, bool, integer, float, string, list and dict",
    ))
}

/// Attributes to write, and the keys whose value is a `datetime`: the registry types
/// those `date`, since JSON has no date of its own.
struct Attributes {
    values: JsonMap<String, Value>,
    dates: Vec<String>,
}

impl Attributes {
    fn of(dict: Option<&Bound<'_, PyDict>>) -> Result<Attributes> {
        let mut attributes = Attributes {
            values: JsonMap::new(),
            dates: Vec::new(),
        };
        for (key, value) in dict.into_iter().flat_map(|dict| dict.iter()) {
            let key = key.extract::<String>()?;
            if value.cast::<PyDateTime>().is_ok() {
                attributes.dates.push(key.clone());
            }
            attributes.values.insert(key, to_json(&value)?);
        }
        Ok(attributes)
    }

    fn register_dates(&self, txn: &mut yrs::TransactionMut<'_>) {
        for key in &self.dates {
            model::register_attribute(txn, key, AttributeType::Date);
        }
    }
}

fn to_json_map(dict: &Bound<'_, PyDict>) -> Result<JsonMap<String, Value>> {
    let mut map = JsonMap::new();
    for (key, value) in dict.iter() {
        map.insert(key.extract::<String>()?, to_json(&value)?);
    }
    Ok(map)
}

/// Says each registry mismatch as a Python warning; the values were written anyway.
fn warn(py: Python<'_>, warnings: &[impl std::fmt::Display]) -> Result<()> {
    let module = py.import("warnings")?;
    for warning in warnings {
        module.call_method1("warn", (warning.to_string(),))?;
    }
    Ok(())
}

fn value_error(error: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(error.to_string())
}

fn to_python(py: Python<'_>, value: &Value) -> Result<Py<PyAny>> {
    Ok(match value {
        Value::Null => py.None(),
        Value::Bool(flag) => flag.into_pyobject(py)?.to_owned().into_any().unbind(),
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                integer.into_pyobject(py)?.into_any().unbind()
            } else {
                number
                    .as_f64()
                    .unwrap_or_default()
                    .into_pyobject(py)?
                    .into_any()
                    .unbind()
            }
        }
        Value::String(text) => text.into_pyobject(py)?.into_any().unbind(),
        Value::Array(items) => {
            let list = PyList::empty(py);
            for item in items {
                list.append(to_python(py, item)?)?;
            }
            list.into_any().unbind()
        }
        Value::Object(map) => {
            let dict = PyDict::new(py);
            for (key, item) in map {
                dict.set_item(key, to_python(py, item)?)?;
            }
            dict.into_any().unbind()
        }
    })
}

fn to_python_map(py: Python<'_>, map: &JsonMap<String, Value>) -> Result<Py<PyAny>> {
    to_python(py, &Value::Object(map.clone()))
}

/// Attributes as Python reads them: a value of a key the registry types `date` comes back
/// as a `datetime`.
fn attributes_to_python(
    py: Python<'_>,
    dates: &[String],
    map: &JsonMap<String, Value>,
) -> Result<Py<PyAny>> {
    let dict = PyDict::new(py);
    for (key, value) in map {
        let date = match value {
            Value::String(text) if dates.contains(key) => Timestamp::parse(text),
            _ => None,
        };
        match date {
            Some(date) => dict.set_item(key, to_python_datetime(py, date)?)?,
            None => dict.set_item(key, to_python(py, value)?)?,
        }
    }
    Ok(dict.into_any().unbind())
}

fn date_keys<T: yrs::ReadTxn>(txn: &T) -> Vec<String> {
    model::attribute_types(txn)
        .into_iter()
        .filter(|(_, info)| info.kind == AttributeType::Date)
        .map(|(key, _)| key)
        .collect()
}

fn optional_datetime(py: Python<'_>, time: Option<Timestamp>) -> Result<Py<PyAny>> {
    match time {
        Some(time) => to_python_datetime(py, time),
        None => Ok(py.None()),
    }
}

fn shape_to_python(py: Python<'_>, shape: &[Vertex]) -> Result<Py<PyAny>> {
    let vertices = PyList::empty(py);
    for vertex in shape {
        vertices.append((to_python_datetime(py, vertex.time)?, vertex.value))?;
    }
    Ok(vertices.into_any().unbind())
}

fn to_shape(value: &Bound<'_, PyAny>) -> Result<Option<Vec<Vertex>>> {
    if value.is_none() {
        return Ok(None);
    }
    let mut shape = Vec::new();
    for vertex in value.try_iter()? {
        let (time, value): (Bound<'_, PyAny>, f64) = vertex?.extract()?;
        shape.push(Vertex {
            time: to_timestamp(&time)?,
            value,
        });
    }
    Ok(Some(shape))
}

fn to_status(name: &str) -> Result<Status> {
    Status::from_name(name).ok_or_else(|| {
        let names: Vec<&str> = Status::ALL.iter().map(|status| status.name()).collect();
        PyValueError::new_err(format!("status is one of {}", names.join(", ")))
    })
}

fn optional_timestamp(value: Option<&Bound<'_, PyAny>>) -> Result<Option<Timestamp>> {
    match value {
        Some(value) if !value.is_none() => Ok(Some(to_timestamp(value)?)),
        _ => Ok(None),
    }
}

/// An event field as the listener's `event_fields` hands it over: times as datetimes.
fn field_to_python(py: Python<'_>, key: &str, value: &Value) -> Result<Py<PyAny>> {
    let nanos = |value: &Value| value.as_i64().and_then(Timestamp::from_unix_nanos);
    match (key, value) {
        ("created", value) => optional_datetime(py, nanos(value)),
        ("shape", Value::Array(vertices)) => {
            let list = PyList::empty(py);
            for vertex in vertices {
                match (
                    vertex.get(0).and_then(nanos),
                    vertex.get(1).and_then(Value::as_f64),
                ) {
                    (Some(time), Some(value)) => {
                        list.append((to_python_datetime(py, time)?, value))?
                    }
                    _ => list.append(to_python(py, vertex)?)?,
                }
            }
            Ok(list.into_any().unbind())
        }
        _ => to_python(py, value),
    }
}

fn uuids(items: Option<&Bound<'_, PyAny>>) -> Result<Vec<String>> {
    let Some(items) = items else {
        return Ok(Vec::new());
    };
    if let Ok(text) = items.extract::<String>() {
        return Ok(vec![text]);
    }
    let mut out = Vec::new();
    for item in items.try_iter()? {
        let item = item?;
        let uuid = match item.cast::<Event>() {
            Ok(event) => event.get().uuid.clone(),
            Err(_) => item
                .extract::<String>()
                .map_err(|_| PyValueError::new_err("expected event uuids or Event objects"))?,
        };
        out.push(uuid);
    }
    Ok(out)
}

fn catalogue_uuid(catalogue: &Bound<'_, PyAny>) -> Result<String> {
    match catalogue.cast::<Catalogue>() {
        Ok(catalogue) => Ok(catalogue.get().uuid.clone()),
        Err(_) => catalogue
            .extract::<String>()
            .map_err(|_| PyValueError::new_err("expected a catalogue uuid or a Catalogue")),
    }
}

/// The rule of the search `text`, catalogues named as the room names them.
fn search_rule(room: &ClientRoom, text: &str, source: Option<&str>) -> Result<Value> {
    let doc = room.doc();
    let txn = doc.transact();
    let predicate = query::parse(text, &model::catalogue_names(&txn)).map_err(value_error)?;
    Ok(query::rule(&predicate, source))
}

fn event_uuid(event: &Bound<'_, PyAny>) -> Result<String> {
    match event.cast::<Event>() {
        Ok(event) => Ok(event.get().uuid.clone()),
        Err(_) => event
            .extract::<String>()
            .map_err(|_| PyValueError::new_err("expected event uuids or Event objects")),
    }
}

fn uuid_argument(uuid: Option<String>) -> Result<String> {
    match uuid {
        Some(uuid) => Ok(uuid),
        None => Ok(uuid::Uuid::new_v4().to_string()),
    }
}

fn now() -> Result<Timestamp> {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(value_error)?;
    i64::try_from(since_epoch.as_nanos())
        .ok()
        .and_then(Timestamp::from_unix_nanos)
        .ok_or_else(|| value_error("the clock is past 2262"))
}

fn hash_of(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn status_name(status: cocat_client::Status) -> &'static str {
    match status {
        cocat_client::Status::Offline => "offline",
        cocat_client::Status::Connecting => "connecting",
        cocat_client::Status::Synced => "synced",
        cocat_client::Status::Unauthorized => "unauthorized",
        cocat_client::Status::NoAccess => "no_access",
    }
}

static NOT_FOUND: PyOnceLock<Py<PyType>> = PyOnceLock::new();

/// The class every lookup of a missing object raises. Python's own mappings raise
/// `LookupError`, and this package raised `RuntimeError` before it named the case,
/// so the class subclasses both.
fn not_found(py: Python<'_>) -> Result<&Bound<'_, PyType>> {
    NOT_FOUND
        .get_or_try_init(py, || {
            let bases = PyTuple::new(
                py,
                [
                    py.get_type::<PyLookupError>(),
                    py.get_type::<PyRuntimeError>(),
                ],
            )?;
            let namespace = PyDict::new(py);
            namespace.set_item("__doc__", "No catalogue or event carries that uuid.")?;
            let class = py
                .import("builtins")?
                .getattr("type")?
                .call1(("NotFound", bases, namespace))?;
            Ok(class.cast_into::<PyType>()?.unbind())
        })
        .map(|class| class.bind(py))
}

fn unknown(what: &str) -> PyErr {
    let message = format!("no {what} with that uuid");
    Python::attach(|py| match not_found(py) {
        Ok(class) => PyErr::from_type(class.clone(), (message,)),
        Err(_) => PyRuntimeError::new_err(message),
    })
}

/// Whether two reads of one catalogue hold the same values. Its member events are not
/// compared: two clients agree on a catalogue's fields before they agree on its contents.
fn same_catalogue(a: &model::CatalogueSummary, b: &model::CatalogueSummary) -> bool {
    let fields = |summary: &model::CatalogueSummary| model::CatalogueSummary {
        event_count: 0,
        ..summary.clone()
    };
    fields(a) == fields(b)
}

#[pyclass(module = "cocat_rs", frozen)]
#[derive(Clone)]
pub struct Catalogue {
    room: Arc<ClientRoom>,
    identity: Arc<Mutex<Identity>>,
    uuid: String,
}

/// What the project's opener knows about where and who it is, for the copies it makes;
/// both stay unknown for a project opened from a folder without a client.
#[derive(Default)]
struct Identity {
    project_name: Option<String>,
    email: Option<String>,
}

impl Catalogue {
    fn of(room: &Arc<ClientRoom>, identity: &Arc<Mutex<Identity>>, uuid: &str) -> Catalogue {
        Catalogue {
            room: Arc::clone(room),
            identity: Arc::clone(identity),
            uuid: uuid.to_owned(),
        }
    }

    fn freeze_hop(&self, name: String, at: Timestamp) -> Hop {
        let identity = lock(&self.identity);
        let project = self.room.room_id().to_owned();
        Hop {
            project_name: identity
                .project_name
                .clone()
                .unwrap_or_else(|| project.clone()),
            project,
            catalogue: self.uuid.clone(),
            catalogue_name: name,
            version: None,
            at: at.unix_millis(),
            by: identity.email.clone(),
        }
    }
}

#[pymethods]
impl Catalogue {
    #[getter]
    fn uuid(&self) -> &str {
        &self.uuid
    }

    fn __hash__(&self) -> u64 {
        hash_of(&self.uuid)
    }

    fn __eq__(&self, other: &Catalogue) -> Result<bool> {
        Ok(same_catalogue(&self.summary()?, &other.summary()?))
    }

    #[getter]
    fn name(&self) -> Result<String> {
        let doc = self.room.doc();
        let txn = doc.transact();
        model::catalogue(&txn, &self.uuid)
            .map(|catalogue| catalogue.name)
            .ok_or_else(|| unknown("catalogue"))
    }

    #[setter]
    fn set_name(&self, name: String) -> Result<()> {
        write(&self.room, |txn| {
            model::rename_catalogue(txn, &self.uuid, &name)
        })?;
        Ok(())
    }

    #[getter]
    fn author(&self) -> Result<Option<String>> {
        Ok(self.summary()?.author)
    }

    #[getter]
    fn created(&self, py: Python<'_>) -> Result<Py<PyAny>> {
        optional_datetime(py, self.summary()?.created)
    }

    #[getter]
    fn deleted(&self) -> Result<bool> {
        Ok(self.summary()?.deleted)
    }

    /// The attribute keys shown as columns, in order.
    #[getter]
    fn columns(&self) -> Result<Vec<String>> {
        Ok(self.summary()?.columns)
    }

    #[setter]
    fn set_columns(&self, columns: Vec<String>) -> Result<()> {
        if !write(&self.room, |txn| {
            model::set_catalogue_columns(txn, &self.uuid, &columns)
        })? {
            return Err(unknown("catalogue"));
        }
        Ok(())
    }

    #[getter]
    fn tags(&self, py: Python<'_>) -> Result<Py<PyAny>> {
        let doc = self.room.doc();
        let txn = doc.transact();
        let catalogue = model::catalogue(&txn, &self.uuid).ok_or_else(|| unknown("catalogue"))?;
        Ok(PySet::new(py, catalogue.tags)?.into_any().unbind())
    }

    #[setter]
    fn set_tags(&self, tags: &Bound<'_, PyAny>) -> Result<()> {
        let tags = uuids(Some(tags))?;
        if !write(&self.room, |txn| {
            model::set_catalogue_tags(txn, &self.uuid, &tags)
        })? {
            return Err(unknown("catalogue"));
        }
        Ok(())
    }

    #[getter]
    fn attributes(&self, py: Python<'_>) -> Result<Py<PyAny>> {
        let doc = self.room.doc();
        let txn = doc.transact();
        let catalogue = model::catalogue(&txn, &self.uuid).ok_or_else(|| unknown("catalogue"))?;
        to_python_map(py, &catalogue.attributes)
    }

    /// What the second coordinate of the shapes measures, as `(quantity, unit)`, or `None`.
    #[getter]
    fn shape_axis(&self) -> Result<Option<(String, String)>> {
        Ok(self
            .summary()?
            .shape_axis
            .map(|axis| (axis.quantity, axis.unit)))
    }

    #[setter]
    fn set_shape_axis(&self, axis: Option<(String, String)>) -> Result<()> {
        let axis = axis.map(|(quantity, unit)| model::ShapeAxis { quantity, unit });
        let found = write(&self.room, |txn| {
            model::set_catalogue_shape_axis(txn, &self.uuid, axis.as_ref())
        })?
        .map_err(value_error)?;
        if !found {
            return Err(unknown("catalogue"));
        }
        Ok(())
    }

    /// The rule that makes this a smart catalogue, or `None`.
    #[getter]
    fn rule(&self, py: Python<'_>) -> Result<Py<PyAny>> {
        match self.summary()?.rule {
            Some(rule) => to_python(py, &rule),
            None => Ok(py.None()),
        }
    }

    /// Sets the rule after checking it; a malformed rule raises `ValueError` naming where it
    /// is wrong.
    #[setter]
    fn set_rule(&self, rule: &Bound<'_, PyAny>) -> Result<()> {
        let rule = match rule.is_none() {
            true => None,
            false => Some(to_json(rule)?),
        };
        self.store_rule(rule.as_ref())
    }

    /// The rule as search text, or `None` when there is no rule or it has no search form
    /// (the time operations: merge, shift, tile, window, closest, set algebra).
    #[getter]
    fn query(&self) -> Result<Option<String>> {
        let doc = self.room.doc();
        let txn = doc.transact();
        let catalogue = model::catalogue(&txn, &self.uuid).ok_or_else(|| unknown("catalogue"))?;
        let names = model::catalogue_names(&txn);
        Ok(catalogue
            .rule
            .and_then(|rule| query::rule_query(&rule, &names))
            .map(|(text, _)| text))
    }

    /// Makes this catalogue the search `text` over all events.
    #[setter]
    fn set_query(&self, text: &str) -> Result<()> {
        let rule = search_rule(&self.room, text, None)?;
        self.store_rule(Some(&rule))
    }

    /// A new static catalogue named `name` holding this catalogue's current members; each
    /// derived interval becomes an event whose origin records the rule.
    fn materialize(&self, py: Python<'_>, name: String) -> Result<Py<Catalogue>> {
        let created = now()?;
        let into = NewCatalogue {
            uuid: uuid_argument(None)?,
            name,
            created: Some(created),
            ..NewCatalogue::default()
        };
        let hop = self.freeze_hop(self.name()?, created);
        let found = write(&self.room, |txn| {
            model::materialize(txn, &self.uuid, &into, hop)
        })?
        .map_err(value_error)?;
        if !found {
            return Err(unknown("catalogue"));
        }
        Py::new(py, Catalogue::of(&self.room, &self.identity, &into.uuid))
    }

    /// The copies this catalogue descends from, oldest first; the last one is its source.
    #[getter]
    fn copied_from<'py>(&self, py: Python<'py>) -> Result<Bound<'py, PyList>> {
        let doc = self.room.doc();
        let chain = model::catalogue(&doc.transact(), &self.uuid)
            .ok_or_else(|| unknown("catalogue"))?
            .copied_from;
        let hops = chain.iter().map(|hop| {
            let dict = PyDict::new(py);
            dict.set_item("project", &hop.project)?;
            dict.set_item("project_name", &hop.project_name)?;
            dict.set_item("catalogue", &hop.catalogue)?;
            dict.set_item("catalogue_name", &hop.catalogue_name)?;
            dict.set_item("version", hop.version)?;
            let at = Timestamp::from_unix_millis(hop.at)
                .map(|at| to_python_datetime(py, at))
                .transpose()?;
            dict.set_item("at", at)?;
            dict.set_item("by", hop.by.as_deref())?;
            Ok::<_, PyErr>(dict)
        });
        PyList::new(py, hops.collect::<Result<Vec<_>>>()?)
    }

    #[getter]
    fn events(&self, py: Python<'_>) -> Result<Py<PyAny>> {
        let records = self.records()?;
        let items: Vec<Py<PyAny>> = records
            .iter()
            .map(|record| Py::new(py, Event::of(&self.room, &record.uuid)).map(Py::into_any))
            .collect::<Result<_>>()?;
        Ok(PySet::new(py, items)?.into_any().unbind())
    }

    #[pyo3(signature = (**attributes))]
    fn set_attributes(&self, attributes: Option<&Bound<'_, PyDict>>) -> Result<()> {
        let Some(attributes) = attributes else {
            return Ok(());
        };
        let attributes = to_json_map(attributes)?;
        write(&self.room, |txn| {
            model::set_catalogue_attributes(txn, &self.uuid, &attributes)
        })?
        .ok()
        .ok_or_else(|| unknown("catalogue"))?;
        Ok(())
    }

    fn remove_attributes(&self, keys: &Bound<'_, PyAny>) -> Result<()> {
        let keys = uuids(Some(keys))?;
        if !write(&self.room, |txn| {
            model::remove_catalogue_attributes(txn, &self.uuid, &keys)
        })? {
            return Err(unknown("catalogue"));
        }
        Ok(())
    }

    fn add_events(&self, events: &Bound<'_, PyAny>) -> Result<()> {
        let events = uuids(Some(events))?;
        if !write(&self.room, |txn| {
            model::add_catalogue_events(txn, &self.uuid, &events)
        })? {
            return Err(unknown("catalogue"));
        }
        Ok(())
    }

    fn remove_events(&self, events: &Bound<'_, PyAny>) -> Result<()> {
        let events = uuids(Some(events))?;
        if !write(&self.room, |txn| {
            model::remove_catalogue_events(txn, &self.uuid, &events)
        })? {
            return Err(unknown("catalogue"));
        }
        Ok(())
    }

    /// Moves the catalogue to the trash; its events stay where they are.
    fn delete(&self) -> Result<()> {
        write(&self.room, |txn| model::delete_catalogue(txn, &self.uuid))?;
        Ok(())
    }

    fn restore(&self) -> Result<()> {
        write(&self.room, |txn| model::restore_catalogue(txn, &self.uuid))?;
        Ok(())
    }

    /// Removes the catalogue for good.
    fn purge(&self) -> Result<()> {
        write(&self.room, |txn| model::purge_catalogue(txn, &self.uuid))?;
        Ok(())
    }

    /// Uuids, start and stop times, and with `attributes=True` each event's attributes, all
    /// from one read of the document, so a remote change cannot fall between them.
    #[pyo3(signature = (attributes=false))]
    fn event_arrays(&self, py: Python<'_>, attributes: bool) -> Result<Py<PyAny>> {
        let (members, dates) = self.members_and_dates()?;
        event_arrays_of(py, &members, &dates, attributes)
    }

    #[pyo3(signature = (keys=None))]
    fn event_attributes(
        &self,
        py: Python<'_>,
        keys: Option<&Bound<'_, PyAny>>,
    ) -> Result<Py<PyAny>> {
        let wanted = match keys {
            Some(keys) => Some(uuids(Some(keys))?),
            None => None,
        };
        let rows = PyList::empty(py);
        let (members, dates) = self.members_and_dates()?;
        for member in &members {
            let map: JsonMap<String, Value> = attributes_of(member)
                .into_iter()
                .flatten()
                .filter(|(key, _)| wanted.as_ref().is_none_or(|keys| keys.contains(key)))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            rows.append(attributes_to_python(py, &dates, &map)?)?;
        }
        Ok(rows.into_any().unbind())
    }
}

impl Catalogue {
    fn store_rule(&self, rule: Option<&Value>) -> Result<()> {
        let found = write(&self.room, |txn| {
            model::set_catalogue_rule(txn, &self.uuid, rule)
        })?
        .map_err(value_error)?;
        if !found {
            return Err(unknown("catalogue"));
        }
        Ok(())
    }

    fn summary(&self) -> Result<model::CatalogueSummary> {
        let doc = self.room.doc();
        let txn = doc.transact();
        model::catalogue(&txn, &self.uuid).ok_or_else(|| unknown("catalogue"))
    }

    fn records(&self) -> Result<Vec<EventRecord>> {
        let doc = self.room.doc();
        let txn = doc.transact();
        model::catalogue_events(&txn, &self.uuid)
            .ok_or_else(|| unknown("catalogue"))?
            .map_err(value_error)
    }

    /// The members and the date-typed attribute keys, from one read.
    fn members_and_dates(&self) -> Result<(Vec<Member>, Vec<String>)> {
        let doc = self.room.doc();
        let txn = doc.transact();
        let members = model::catalogue_members(&txn, &self.uuid)
            .ok_or_else(|| unknown("catalogue"))?
            .map_err(value_error)?;
        Ok((members, date_keys(&txn)))
    }
}

#[pyclass(module = "cocat_rs", frozen)]
#[derive(Clone)]
pub struct Event {
    room: Arc<ClientRoom>,
    uuid: String,
}

impl Event {
    fn of(room: &Arc<ClientRoom>, uuid: &str) -> Event {
        Event {
            room: Arc::clone(room),
            uuid: uuid.to_owned(),
        }
    }

    fn record(&self) -> Result<EventRecord> {
        let doc = self.room.doc();
        let txn = doc.transact();
        model::event(&txn, &self.uuid).ok_or_else(|| unknown("event"))
    }

    fn set_field(&self, field: EventField) -> Result<()> {
        let found = write(&self.room, |txn| {
            model::set_event_field(txn, &self.uuid, &field)
        })?
        .map_err(value_error)?;
        if !found {
            return Err(unknown("event"));
        }
        Ok(())
    }
}

#[pymethods]
impl Event {
    #[getter]
    fn uuid(&self) -> &str {
        &self.uuid
    }

    fn __hash__(&self) -> u64 {
        hash_of(&self.uuid)
    }

    fn __eq__(&self, other: &Event) -> Result<bool> {
        Ok(self.record()? == other.record()?)
    }

    #[getter]
    fn start(&self, py: Python<'_>) -> Result<Py<PyAny>> {
        to_python_datetime(py, self.record()?.start)
    }

    #[getter]
    fn stop(&self, py: Python<'_>) -> Result<Py<PyAny>> {
        to_python_datetime(py, self.record()?.stop)
    }

    #[getter]
    fn range(&self, py: Python<'_>) -> Result<Py<PyAny>> {
        let record = self.record()?;
        let pair = (
            to_python_datetime(py, record.start)?,
            to_python_datetime(py, record.stop)?,
        );
        Ok(pair.into_pyobject(py)?.into_any().unbind())
    }

    #[setter]
    fn set_range(&self, range: &Bound<'_, PyAny>) -> Result<()> {
        let (start, stop): (Bound<'_, PyAny>, Bound<'_, PyAny>) = range.extract()?;
        let start = to_timestamp(&start)?;
        let stop = to_timestamp(&stop)?;
        let found = write(&self.room, |txn| {
            model::set_event_range(txn, &self.uuid, start, stop)
        })?
        .map_err(value_error)?;
        if !found {
            return Err(unknown("event"));
        }
        Ok(())
    }

    #[getter]
    fn author(&self) -> Result<Option<String>> {
        Ok(self.record()?.author)
    }

    #[setter]
    fn set_author(&self, author: Option<String>) -> Result<()> {
        self.set_field(EventField::Author(author))
    }

    #[getter]
    fn status(&self) -> Result<&'static str> {
        Ok(self.record()?.status.name())
    }

    #[setter]
    fn set_status(&self, status: &str) -> Result<()> {
        self.set_field(EventField::Status(to_status(status)?))
    }

    #[getter]
    fn confidence(&self) -> Result<Option<f64>> {
        Ok(self.record()?.confidence)
    }

    #[setter]
    fn set_confidence(&self, confidence: Option<f64>) -> Result<()> {
        self.set_field(EventField::Confidence(confidence))
    }

    #[getter]
    fn start_err(&self) -> Result<Option<f64>> {
        Ok(self.record()?.start_err)
    }

    #[setter]
    fn set_start_err(&self, seconds: Option<f64>) -> Result<()> {
        self.set_field(EventField::StartErr(seconds))
    }

    #[getter]
    fn stop_err(&self) -> Result<Option<f64>> {
        Ok(self.record()?.stop_err)
    }

    #[setter]
    fn set_stop_err(&self, seconds: Option<f64>) -> Result<()> {
        self.set_field(EventField::StopErr(seconds))
    }

    #[getter]
    fn origin(&self) -> Result<Option<String>> {
        Ok(self.record()?.origin)
    }

    #[setter]
    fn set_origin(&self, origin: Option<String>) -> Result<()> {
        self.set_field(EventField::Origin(origin))
    }

    #[getter]
    fn source_id(&self) -> Result<Option<String>> {
        Ok(self.record()?.source_id)
    }

    #[setter]
    fn set_source_id(&self, source_id: Option<String>) -> Result<()> {
        self.set_field(EventField::SourceId(source_id))
    }

    #[getter]
    fn created(&self, py: Python<'_>) -> Result<Py<PyAny>> {
        optional_datetime(py, self.record()?.created)
    }

    #[setter]
    fn set_created(&self, created: &Bound<'_, PyAny>) -> Result<()> {
        self.set_field(EventField::Created(optional_timestamp(Some(created))?))
    }

    /// The shape as `(datetime, value)` pairs, the value on the catalogue's shape axis, or
    /// `None`.
    #[getter]
    fn shape(&self, py: Python<'_>) -> Result<Py<PyAny>> {
        match self.record()?.shape {
            Some(shape) => shape_to_python(py, &shape),
            None => Ok(py.None()),
        }
    }

    #[setter]
    fn set_shape(&self, shape: &Bound<'_, PyAny>) -> Result<()> {
        self.set_field(EventField::Shape(to_shape(shape)?))
    }

    #[getter]
    fn comment(&self) -> Result<Option<String>> {
        let doc = self.room.doc();
        let txn = doc.transact();
        model::event(&txn, &self.uuid).ok_or_else(|| unknown("event"))?;
        Ok(model::comment(&txn, &self.uuid))
    }

    #[setter]
    fn set_comment(&self, comment: Option<String>) -> Result<()> {
        if !write(&self.room, |txn| {
            model::set_comment(txn, &self.uuid, comment.as_deref())
        })? {
            return Err(unknown("event"));
        }
        Ok(())
    }

    #[getter]
    fn tags(&self, py: Python<'_>) -> Result<Py<PyAny>> {
        Ok(PySet::new(py, self.record()?.tags)?.into_any().unbind())
    }

    #[setter]
    fn set_tags(&self, tags: &Bound<'_, PyAny>) -> Result<()> {
        self.set_field(EventField::Tags(uuids(Some(tags))?))
    }

    #[getter]
    fn products(&self, py: Python<'_>) -> Result<Py<PyAny>> {
        Ok(PySet::new(py, self.record()?.products)?.into_any().unbind())
    }

    #[setter]
    fn set_products(&self, products: &Bound<'_, PyAny>) -> Result<()> {
        self.set_field(EventField::Products(uuids(Some(products))?))
    }

    #[getter]
    fn attributes(&self, py: Python<'_>) -> Result<Py<PyAny>> {
        let doc = self.room.doc();
        let txn = doc.transact();
        let record = model::event(&txn, &self.uuid).ok_or_else(|| unknown("event"))?;
        attributes_to_python(py, &date_keys(&txn), &record.attributes)
    }

    /// The uuids of the catalogues that list this event, sorted.
    #[getter]
    fn catalogues(&self, py: Python<'_>) -> Result<Py<PyAny>> {
        let doc = self.room.doc();
        let txn = doc.transact();
        let uuids = model::event_catalogues(&txn, &self.uuid);
        Ok(PyList::new(py, uuids)?.into_any().unbind())
    }

    #[pyo3(signature = (**attributes))]
    fn set_attributes(&self, py: Python<'_>, attributes: Option<&Bound<'_, PyDict>>) -> Result<()> {
        let attributes = Attributes::of(attributes)?;
        let warnings = write(&self.room, |txn| {
            attributes.register_dates(txn);
            model::set_event_attributes(txn, &self.uuid, &attributes.values)
        })?
        .map_err(value_error)?
        .ok_or_else(|| unknown("event"))?;
        warn(py, &warnings)
    }

    fn remove_attributes(&self, keys: &Bound<'_, PyAny>) -> Result<()> {
        let keys = uuids(Some(keys))?;
        if !write(&self.room, |txn| {
            model::remove_event_attributes(txn, &self.uuid, &keys)
        })? {
            return Err(unknown("event"));
        }
        Ok(())
    }

    /// Moves the event to the trash: catalogues stop showing it until it is restored.
    fn delete(&self) -> Result<()> {
        write(&self.room, |txn| model::delete_event(txn, &self.uuid))?;
        Ok(())
    }

    fn restore(&self) -> Result<()> {
        write(&self.room, |txn| model::restore_event(txn, &self.uuid))?;
        Ok(())
    }

    /// Removes the event for good, from the project and from every catalogue.
    fn purge(&self) -> Result<()> {
        write(&self.room, |txn| model::purge_event(txn, &self.uuid))?;
        Ok(())
    }
}

/// The arrays Python scripts plot with, in the order the members come. A derived interval
/// has no uuid and names the events it came from in `sources`; an event's `sources` is `None`.
fn event_arrays_of(
    py: Python<'_>,
    members: &[Member],
    dates: &[String],
    attributes: bool,
) -> Result<Py<PyAny>> {
    let (uuids, sources): (Vec<_>, Vec<_>) = members
        .iter()
        .map(|member| match member {
            Member::Event { record, .. } => (Some(record.uuid.as_str()), None),
            Member::Derived { sources, .. } => (None, Some(sources)),
        })
        .unzip();
    let micros = |time: Timestamp| time.unix_micros() as i64;
    let (starts, stops) = members
        .iter()
        .map(span)
        .map(|(start, stop)| (micros(start), micros(stop)))
        .unzip();
    let arrays = PyDict::new(py);
    arrays.set_item("uuid", uuids)?;
    arrays.set_item("start", datetime_array(py, starts)?)?;
    arrays.set_item("stop", datetime_array(py, stops)?)?;
    arrays.set_item("sources", sources)?;
    if attributes {
        let rows = PyList::empty(py);
        for member in members {
            let empty = JsonMap::new();
            rows.append(attributes_to_python(
                py,
                dates,
                attributes_of(member).unwrap_or(&empty),
            )?)?;
        }
        arrays.set_item("attributes", rows)?;
    }
    Ok(arrays.into_any().unbind())
}

fn span(member: &Member) -> (Timestamp, Timestamp) {
    match member {
        Member::Event { record, .. } => (record.start, record.stop),
        Member::Derived { start, stop, .. } => (*start, *stop),
    }
}

/// An event's attributes; a derived interval has none.
fn attributes_of(member: &Member) -> Option<&JsonMap<String, Value>> {
    match member {
        Member::Event { record, .. } => Some(&record.attributes),
        Member::Derived { .. } => None,
    }
}

fn datetime_array(py: Python<'_>, micros: Vec<i64>) -> Result<Py<PyAny>> {
    let array = PyArray1::from_vec(py, micros);
    Ok(array.call_method1("view", ("datetime64[us]",))?.unbind())
}

#[pyclass(module = "cocat_rs")]
pub struct Project {
    room: Arc<ClientRoom>,
    batches: Mutex<Receiver<Vec<Change>>>,
    url: Mutex<Option<String>>,
    credentials: Mutex<Option<(String, String)>>,
    identity: Arc<Mutex<Identity>>,
}

impl Project {
    fn of(room: ClientRoom) -> Project {
        let room = Arc::new(room);
        let (sender, batches) = mpsc::channel();
        room.subscribe(move |batch| {
            let _ = sender.send(batch);
        });
        Project {
            room,
            batches: Mutex::new(batches),
            url: Mutex::new(None),
            credentials: Mutex::new(None),
            identity: Arc::default(),
        }
    }
}

/// How many catalogues and events the project holds, the trash included.
fn root_sizes(room: &ClientRoom) -> (u32, u32) {
    let doc = room.doc();
    let txn = doc.transact();
    let size = |name| txn.get_map(name).map_or(0, |map| map.len(&txn));
    (size(model::CATALOGUES), size(model::EVENTS))
}

#[pymethods]
impl Project {
    #[staticmethod]
    fn open(cache_dir: String, project_id: String) -> Result<Project> {
        std::fs::create_dir_all(Path::new(&cache_dir))
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        let room = ClientRoom::open(Path::new(&cache_dir), &project_id)
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        Ok(Project::of(room))
    }

    /// The project as of a version, read-only and never connected: once the state is fetched,
    /// the server is not needed.
    #[staticmethod]
    fn at(
        py: Python<'_>,
        url: String,
        cookie: String,
        project_id: String,
        version: u64,
    ) -> Result<Project> {
        let state = py
            .detach(|| {
                block_on(cocat_client::version_state(
                    &url,
                    &cookie,
                    &project_id,
                    version,
                ))
            })?
            .map_err(http_error)?;
        let room = ClientRoom::past(&project_id, &state)
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        Ok(Project::of(room))
    }

    /// Connects with a session cookie, or with `(username, password)`, which logs in first.
    /// The credentials are kept so a session the server refuses can log in again;
    /// `set_cookie` reuses them.
    #[pyo3(signature = (url, cookie=None, credentials=None))]
    fn connect(
        &self,
        py: Python<'_>,
        url: String,
        cookie: Option<String>,
        credentials: Option<(String, String)>,
    ) -> Result<()> {
        let cookie = match (cookie, &credentials) {
            (Some(cookie), _) => cookie,
            (None, Some((username, password))) => {
                login(py, url.clone(), username.clone(), password.clone())?
            }
            (None, None) => {
                return Err(PyValueError::new_err(
                    "connect needs a cookie or credentials",
                ));
            }
        };
        let login = credentials
            .clone()
            .map(|(username, password)| Credentials::new(username, password));
        self.room
            .connect_with(&url, &cookie, login)
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        *lock(&self.url) = Some(url);
        *lock(&self.credentials) = credentials;
        Ok(())
    }

    fn set_cookie(&self, cookie: String) -> Result<()> {
        let Some(url) = lock(&self.url).clone() else {
            return Err(PyRuntimeError::new_err("this project never connected"));
        };
        let login = lock(&self.credentials)
            .clone()
            .map(|(username, password)| Credentials::new(username, password));
        self.room
            .connect_with(&url, &cookie, login)
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))
    }

    /// The project's name and the signed-in email, recorded in the copies made from here.
    fn set_identity(&self, project_name: String, email: String) {
        *lock(&self.identity) = Identity {
            project_name: Some(project_name),
            email: Some(email),
        };
    }

    fn close(&self) {
        self.room.close();
    }

    #[getter]
    fn status(&self) -> &'static str {
        status_name(self.room.status())
    }

    /// The role the server gave this account at the last connect; `None` before any.
    #[getter]
    fn role(&self) -> Option<&'static str> {
        self.room.role().map(|role| role.as_str())
    }

    /// Whether writes are refused: a past version, or a viewer's project.
    #[getter]
    fn read_only(&self) -> bool {
        self.room.is_read_only()
    }

    #[pyo3(signature = (timeout=None))]
    fn wait_synced(&self, py: Python<'_>, timeout: Option<f64>) -> Result<()> {
        // A year is long enough to mean "until it happens" and short enough to be a duration.
        let timeout = timeout.map_or(
            Duration::from_secs(365 * 24 * 3600),
            Duration::from_secs_f64,
        );
        py.detach(|| self.room.wait_synced(timeout))
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))
    }

    /// The next batch of remote changes, as dicts for `cocat_rs/__init__.py`, or `None`
    /// when `timeout` passes first.
    #[pyo3(signature = (timeout=None))]
    fn next_batch(&self, py: Python<'_>, timeout: Option<f64>) -> Result<Option<Vec<Py<PyAny>>>> {
        let batch = py.detach(|| match timeout {
            Some(seconds) => {
                let batches = lock(&self.batches);
                match batches.recv_timeout(Duration::from_secs_f64(seconds)) {
                    Ok(batch) => Some(batch),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => None,
                }
            }
            None => lock(&self.batches).recv().ok(),
        });
        match batch {
            Some(batch) => {
                let items = batch
                    .iter()
                    .map(|change| change_to_python(py, change))
                    .collect::<Result<Vec<_>>>()?;
                Ok(Some(items))
            }
            None => Ok(None),
        }
    }

    fn begin_batch(&self) {
        self.room.begin_batch();
    }

    fn end_batch(&self) {
        self.room.end_batch();
    }

    fn catalogues(&self, py: Python<'_>) -> Result<Vec<Py<Catalogue>>> {
        let doc = self.room.doc();
        let txn = doc.transact();
        model::catalogues(&txn)
            .iter()
            .map(|catalogue| {
                Py::new(
                    py,
                    Catalogue::of(&self.room, &self.identity, &catalogue.uuid),
                )
            })
            .collect()
    }

    fn catalogue(&self, py: Python<'_>, uuid_or_name: &str) -> Result<Py<Catalogue>> {
        let doc = self.room.doc();
        let txn = doc.transact();
        let found = model::catalogue(&txn, uuid_or_name).or_else(|| {
            model::catalogues(&txn)
                .into_iter()
                .find(|catalogue| catalogue.name == uuid_or_name)
        });
        let catalogue = found.ok_or_else(|| unknown("catalogue"))?;
        Py::new(
            py,
            Catalogue::of(&self.room, &self.identity, &catalogue.uuid),
        )
    }

    fn event(&self, py: Python<'_>, uuid: &str) -> Result<Py<Event>> {
        let doc = self.room.doc();
        let txn = doc.transact();
        model::event(&txn, uuid).ok_or_else(|| unknown("event"))?;
        Py::new(py, Event::of(&self.room, uuid))
    }

    /// The arrays of the events named (uuids or `Event`s), in the order given and from one
    /// read of the document. A uuid the project does not hold is left out, and the returned
    /// `uuid` list says which came back.
    #[pyo3(signature = (events, attributes=false))]
    fn event_arrays(
        &self,
        py: Python<'_>,
        events: &Bound<'_, PyAny>,
        attributes: bool,
    ) -> Result<Py<PyAny>> {
        let wanted = uuids(Some(events))?;
        let records = {
            let doc = self.room.doc();
            let txn = doc.transact();
            let records = wanted
                .iter()
                .filter_map(|uuid| model::event(&txn, uuid))
                .map(|record| Member::Event { record, gap: None })
                .collect::<Vec<_>>();
            (records, date_keys(&txn))
        };
        event_arrays_of(py, &records.0, &records.1, attributes)
    }

    /// The events not in the trash that the search `query` matches, in time order.
    fn search(&self, py: Python<'_>, query: &str) -> Result<Vec<Py<Event>>> {
        let records = {
            let doc = self.room.doc();
            let txn = doc.transact();
            let predicate =
                query::parse(query, &model::catalogue_names(&txn)).map_err(value_error)?;
            model::search(&txn, &predicate, now()?).map_err(value_error)?
        };
        records
            .iter()
            .map(|record| Py::new(py, Event::of(&self.room, &record.uuid)))
            .collect()
    }

    /// A smart catalogue whose rule is the search `query` over all events, or over the
    /// events of `source` (a `Catalogue` or a uuid).
    #[pyo3(signature = (name, query, source=None))]
    fn create_smart_catalogue(
        &self,
        py: Python<'_>,
        name: String,
        query: &str,
        source: Option<&Bound<'_, PyAny>>,
    ) -> Result<Py<Catalogue>> {
        let source = source.map(catalogue_uuid).transpose()?;
        let rule = search_rule(&self.room, query, source.as_deref())?;
        let catalogue = NewCatalogue {
            uuid: uuid_argument(None)?,
            name,
            created: Some(now()?),
            ..NewCatalogue::default()
        };
        write(&self.room, |txn| {
            model::create_catalogue(txn, &catalogue)?;
            model::set_catalogue_rule(txn, &catalogue.uuid, Some(&rule))
        })?
        .map_err(value_error)?;
        Py::new(
            py,
            Catalogue::of(&self.room, &self.identity, &catalogue.uuid),
        )
    }

    /// The events in the trash, in time order.
    fn trash(&self, py: Python<'_>) -> Result<Vec<Py<Event>>> {
        let doc = self.room.doc();
        let txn = doc.transact();
        model::trash(&txn)
            .iter()
            .map(|record| Py::new(py, Event::of(&self.room, &record.uuid)))
            .collect()
    }

    /// Every tag a live event or catalogue carries, sorted.
    fn existing_tags(&self) -> Vec<String> {
        model::existing_tags(&self.room.doc().transact())
    }

    /// The attribute registry: key to `{"type", "unit", "description"}`.
    fn attribute_types(&self, py: Python<'_>) -> Result<Py<PyAny>> {
        let types = PyDict::new(py);
        for (key, info) in model::attribute_types(&self.room.doc().transact()) {
            let entry = PyDict::new(py);
            entry.set_item("type", info.kind.name())?;
            entry.set_item("unit", info.unit)?;
            entry.set_item("description", info.description)?;
            types.set_item(key, entry)?;
        }
        Ok(types.into_any().unbind())
    }

    /// Sets the unit and description of a registered key; `None` keeps a field, `""`
    /// removes it.
    #[pyo3(signature = (key, unit=None, description=None))]
    fn set_attribute_info(
        &self,
        key: &str,
        unit: Option<String>,
        description: Option<String>,
    ) -> Result<()> {
        if !write(&self.room, |txn| {
            model::set_attribute_info(txn, key, unit.as_deref(), description.as_deref())
        })? {
            return Err(PyLookupError::new_err(format!(
                "no attribute {key} is registered"
            )));
        }
        Ok(())
    }

    /// Records what made a set of events and returns its id, for their `origin`.
    #[pyo3(signature = (kind, name, version=None, author=None, created=None, used=None, parameters=None, derived_from=None, id=None))]
    #[allow(clippy::too_many_arguments)]
    fn create_origin(
        &self,
        kind: &str,
        name: String,
        version: Option<String>,
        author: Option<String>,
        created: Option<&Bound<'_, PyAny>>,
        used: Option<&Bound<'_, PyAny>>,
        parameters: Option<&Bound<'_, PyAny>>,
        derived_from: Option<String>,
        id: Option<String>,
    ) -> Result<String> {
        let kind: OriginKind = serde_json::from_value(Value::from(kind))
            .map_err(|_| value_error("kind is one of rule, detector, model, import"))?;
        let used: Vec<Used> = match used {
            Some(used) => serde_json::from_value(to_json(used)?).map_err(value_error)?,
            None => Vec::new(),
        };
        let origin = Origin {
            id: uuid_argument(id)?,
            kind,
            name,
            version,
            author,
            created: optional_timestamp(created)?,
            used,
            parameters: parameters.map(to_json).transpose()?,
            derived_from,
        };
        write(&self.room, |txn| model::create_origin(txn, &origin))?.map_err(value_error)?;
        Ok(origin.id)
    }

    /// One origin as a dict, or `NotFound`.
    fn origin(&self, py: Python<'_>, id: &str) -> Result<Py<PyAny>> {
        let origin =
            model::origin(&self.room.doc().transact(), id).ok_or_else(|| unknown("origin"))?;
        let dict = PyDict::new(py);
        dict.set_item("id", origin.id)?;
        dict.set_item(
            "kind",
            to_python(py, &serde_json::to_value(origin.kind).map_err(value_error)?)?,
        )?;
        dict.set_item("name", origin.name)?;
        dict.set_item("version", origin.version)?;
        dict.set_item("author", origin.author)?;
        dict.set_item("created", optional_datetime(py, origin.created)?)?;
        dict.set_item(
            "used",
            to_python(
                py,
                &serde_json::to_value(&origin.used).map_err(value_error)?,
            )?,
        )?;
        dict.set_item(
            "parameters",
            to_python(py, &origin.parameters.unwrap_or(Value::Null))?,
        )?;
        dict.set_item("derived_from", origin.derived_from)?;
        Ok(dict.into_any().unbind())
    }

    #[pyo3(signature = (name, author=None, uuid=None, tags=None, attributes=None, events=None, columns=None))]
    #[allow(clippy::too_many_arguments)]
    fn create_catalogue(
        &self,
        py: Python<'_>,
        name: String,
        author: Option<String>,
        uuid: Option<String>,
        tags: Option<&Bound<'_, PyAny>>,
        attributes: Option<&Bound<'_, PyDict>>,
        events: Option<&Bound<'_, PyAny>>,
        columns: Option<Vec<String>>,
    ) -> Result<Py<Catalogue>> {
        let uuid = uuid_argument(uuid)?;
        let catalogue = NewCatalogue {
            uuid: uuid.clone(),
            name,
            author,
            created: Some(now()?),
            tags: uuids(tags)?,
            columns: columns.unwrap_or_default(),
            attributes: match attributes {
                Some(attributes) => to_json_map(attributes)?,
                None => JsonMap::new(),
            },
        };
        write(&self.room, |txn| model::create_catalogue(txn, &catalogue))?.map_err(value_error)?;
        if let Some(events) = events {
            let mut uuids = Vec::new();
            for event in events.try_iter()? {
                uuids.push(event_uuid(&event?)?);
            }
            write(&self.room, |txn| {
                model::add_catalogue_events(txn, &uuid, &uuids)
            })?;
        }
        Py::new(py, Catalogue::of(&self.room, &self.identity, &uuid))
    }

    /// A hand-made event (no `origin`) records when it was created unless told otherwise.
    #[pyo3(signature = (
        start, stop, author=None, uuid=None, tags=None, products=None, attributes=None,
        status=None, confidence=None, start_err=None, stop_err=None, origin=None,
        source_id=None, created=None, comment=None, shape=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn create_event(
        &self,
        py: Python<'_>,
        start: &Bound<'_, PyAny>,
        stop: &Bound<'_, PyAny>,
        author: Option<String>,
        uuid: Option<String>,
        tags: Option<&Bound<'_, PyAny>>,
        products: Option<&Bound<'_, PyAny>>,
        attributes: Option<&Bound<'_, PyDict>>,
        status: Option<&str>,
        confidence: Option<f64>,
        start_err: Option<f64>,
        stop_err: Option<f64>,
        origin: Option<String>,
        source_id: Option<String>,
        created: Option<&Bound<'_, PyAny>>,
        comment: Option<String>,
        shape: Option<&Bound<'_, PyAny>>,
    ) -> Result<Py<Event>> {
        let mut event = EventRecord::new(
            uuid_argument(uuid)?,
            to_timestamp(start)?,
            to_timestamp(stop)?,
        );
        event.created = match (optional_timestamp(created)?, &origin) {
            (Some(created), _) => Some(created),
            (None, None) => Some(now()?),
            (None, Some(_)) => None,
        };
        event.author = author;
        event.tags = uuids(tags)?;
        event.products = uuids(products)?;
        event.status = status.map(to_status).transpose()?.unwrap_or_default();
        event.confidence = confidence;
        event.start_err = start_err;
        event.stop_err = stop_err;
        event.origin = origin;
        event.source_id = source_id;
        event.shape = shape.map(to_shape).transpose()?.flatten();
        let attributes = Attributes::of(attributes)?;
        event.attributes = attributes.values.clone();
        let warnings = write(&self.room, |txn| {
            attributes.register_dates(txn);
            let warnings = model::create_event(txn, &event)?;
            model::set_comment(txn, &event.uuid, comment.as_deref());
            Ok::<_, model::WriteError>(warnings)
        })?
        .map_err(value_error)?;
        warn(py, &warnings)?;
        Py::new(py, Event::of(&self.room, &event.uuid))
    }

    /// Brings in what `source` holds, as one edit of this project, and says how many
    /// catalogues and events were new here.
    fn merge(&self, py: Python<'_>, source: PyRef<'_, Project>) -> Result<Py<PyAny>> {
        let before = root_sizes(&self.room);
        self.room
            .merge(&source.room)
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        let after = root_sizes(&self.room);
        let added = PyDict::new(py);
        added.set_item("catalogues", after.0.saturating_sub(before.0))?;
        added.set_item("events", after.1.saturating_sub(before.1))?;
        Ok(added.into_any().unbind())
    }

    /// Blocks until the server has processed every change made so far, or `timeout` passes.
    #[pyo3(signature = (timeout=None))]
    fn save(&self, py: Python<'_>, timeout: Option<f64>) -> Result<()> {
        let timeout = timeout.map_or(DEFAULT_SAVE_TIMEOUT, Duration::from_secs_f64);
        py.detach(|| self.room.save(timeout))
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))
    }

    /// Imports the VOTable's tables into this project and returns how many catalogues it added.
    #[pyo3(signature = (path, table_name=None))]
    fn import_votable(
        &self,
        py: Python<'_>,
        path: String,
        table_name: Option<String>,
    ) -> Result<usize> {
        let xml = std::fs::read_to_string(&path).map_err(value_error)?;
        let imported = votable::import(&xml).map_err(value_error)?;
        let written = write(&self.room, |txn| {
            votable::write(txn, &imported, table_name.as_deref())
        })?
        .map_err(value_error)?;
        warn(py, &written.warnings)?;
        Ok(written.catalogues.len())
    }

    /// Imports a TFCat file as a new catalogue and returns it. The catalogue is named `name`,
    /// else by the file's title, else by the file's name. What could not come across as it
    /// was is said as warnings.
    #[pyo3(signature = (path, name=None))]
    fn import_tfcat(
        &self,
        py: Python<'_>,
        path: String,
        name: Option<String>,
    ) -> Result<Py<Catalogue>> {
        let text = std::fs::read_to_string(&path).map_err(value_error)?;
        let import = tfcat::parse(&text).map_err(value_error)?;
        let name = import.name(name.as_deref(), &path);
        let now = now()?;
        let written = write(&self.room, |txn| tfcat::write(txn, &import, &name, now))?
            .map_err(value_error)?;
        warn(py, &import.warnings)?;
        warn(py, &written.warnings)?;
        let uuid = written
            .catalogues
            .first()
            .ok_or_else(|| unknown("catalogue"))?;
        Py::new(py, Catalogue::of(&self.room, &self.identity, uuid))
    }

    /// Imports a file in tscat's JSON format and returns the uuids of its catalogues, in the
    /// file's order. What could not come across as it was is said as warnings.
    fn import_tscat_json(&self, py: Python<'_>, text: &str) -> Result<Vec<String>> {
        let import = tscat_json::parse(text).map_err(value_error)?;
        let now = now()?;
        let written =
            write(&self.room, |txn| tscat_json::write(txn, &import, now))?.map_err(value_error)?;
        warn(py, &import.warnings)?;
        warn(py, &written.warnings)?;
        Ok(written.catalogues)
    }
}

/// The VOTable a set of catalogues exports to, the file Python cocat's exporter writes.
///
/// The catalogues carry their project, so the script mode can export a project it never
/// logged into.
#[pyfunction]
fn export_votable(catalogues: &Bound<'_, PyAny>) -> Result<String> {
    let (room, uuids) = catalogues_of(catalogues)?;
    let doc = room.doc();
    let txn = doc.transact();
    let exported = catalogues_with_events(&txn, &uuids)?;
    let pairs: Vec<(&model::CatalogueSummary, &[EventRecord])> = exported
        .iter()
        .map(|(catalogue, events)| (catalogue, events.as_slice()))
        .collect();
    votable::export_many(&pairs).map_err(|error| PyValueError::new_err(error.to_string()))
}

/// Each catalogue with its member events, for an export.
fn catalogues_with_events<T: yrs::ReadTxn>(
    txn: &T,
    uuids: &[String],
) -> Result<Vec<(model::CatalogueSummary, Vec<EventRecord>)>> {
    let mut exported = Vec::new();
    for uuid in uuids {
        let catalogue = model::catalogue(txn, uuid).ok_or_else(|| unknown("catalogue"))?;
        let events = model::catalogue_events(txn, uuid)
            .unwrap_or_else(|| Ok(Vec::new()))
            .map_err(value_error)?;
        exported.push((catalogue, events));
    }
    Ok(exported)
}

/// The catalogue as TFCat JSON. Events with no place on the shape axis are drawn over
/// `value_range` (low, high), else over the band of the catalogue's other events.
/// A shape axis TFCat does not know is said as a warning: the file is not strict TFCat.
#[pyfunction]
#[pyo3(signature = (catalogue, value_range=None))]
fn tfcat_json(catalogue: PyRef<'_, Catalogue>, value_range: Option<(f64, f64)>) -> Result<String> {
    let doc = catalogue.room.doc();
    let txn = doc.transact();
    let exported = catalogues_with_events(&txn, std::slice::from_ref(&catalogue.uuid))?;
    let (summary, events) = exported.first().ok_or_else(|| unknown("catalogue"))?;
    let text = tfcat::export(&txn, summary, events, value_range).map_err(value_error)?;
    let warning = tfcat::strictness_warning(summary);
    warn(catalogue.py(), warning.as_slice())?;
    Ok(text)
}

/// The catalogues and their member events in tscat's JSON format.
#[pyfunction]
fn export_tscat_json(catalogues: &Bound<'_, PyAny>) -> Result<String> {
    let (room, uuids) = catalogues_of(catalogues)?;
    let doc = room.doc();
    let txn = doc.transact();
    let exported = catalogues_with_events(&txn, &uuids)?;
    let pairs: Vec<(&model::CatalogueSummary, &[EventRecord])> = exported
        .iter()
        .map(|(catalogue, events)| (catalogue, events.as_slice()))
        .collect();
    tscat_json::export(&txn, &pairs).map_err(value_error)
}

/// The uuids of the catalogues an argument names (one catalogue or an iterable of them),
/// with the project they belong to.
fn catalogues_of(value: &Bound<'_, PyAny>) -> Result<(Arc<ClientRoom>, Vec<String>)> {
    if let Ok(catalogue) = value.extract::<PyRef<Catalogue>>() {
        return Ok((Arc::clone(&catalogue.room), vec![catalogue.uuid.clone()]));
    }
    let mut room = None;
    let mut uuids = Vec::new();
    for catalogue in value.try_iter()? {
        let catalogue = catalogue?.extract::<PyRef<Catalogue>>()?;
        room.get_or_insert_with(|| Arc::clone(&catalogue.room));
        uuids.push(catalogue.uuid.clone());
    }
    let room = room.ok_or_else(|| PyValueError::new_err("no catalogue to export"))?;
    Ok((room, uuids))
}

fn change_to_python(py: Python<'_>, change: &Change) -> Result<Py<PyAny>> {
    let dict = PyDict::new(py);
    match change {
        Change::CatalogueAdded { uuid } => {
            dict.set_item("kind", "catalogue_added")?;
            dict.set_item("uuid", uuid)?;
        }
        Change::CatalogueRemoved { uuid } => {
            dict.set_item("kind", "catalogue_removed")?;
            dict.set_item("uuid", uuid)?;
        }
        Change::CatalogueRenamed { uuid, name } => {
            dict.set_item("kind", "catalogue_renamed")?;
            dict.set_item("uuid", uuid)?;
            dict.set_item("name", name)?;
        }
        Change::CatalogueAttributes { uuid, set, removed } => {
            dict.set_item("kind", "catalogue_attributes")?;
            dict.set_item("uuid", uuid)?;
            dict.set_item("set", to_python_map(py, set)?)?;
            dict.set_item("removed", removed)?;
        }
        Change::EventsAdded { catalogue, uuids } => {
            dict.set_item("kind", "events_added")?;
            dict.set_item("catalogue", catalogue)?;
            dict.set_item("uuids", uuids)?;
        }
        Change::EventsRemoved { catalogue, uuids } => {
            dict.set_item("kind", "events_removed")?;
            dict.set_item("catalogue", catalogue)?;
            dict.set_item("uuids", uuids)?;
        }
        Change::EventRange { uuid, start, stop } => {
            dict.set_item("kind", "event_range")?;
            dict.set_item("uuid", uuid)?;
            dict.set_item("start", to_python_datetime(py, *start)?)?;
            dict.set_item("stop", to_python_datetime(py, *stop)?)?;
        }
        Change::EventAttributes { uuid, set, removed } => {
            dict.set_item("kind", "event_attributes")?;
            dict.set_item("uuid", uuid)?;
            dict.set_item("set", to_python_map(py, set)?)?;
            dict.set_item("removed", removed)?;
        }
        Change::EventRemoved { uuid } => {
            dict.set_item("kind", "event_removed")?;
            dict.set_item("uuid", uuid)?;
        }
        Change::EventFields { uuid, set, removed } => {
            dict.set_item("kind", "event_fields")?;
            dict.set_item("uuid", uuid)?;
            let fields = PyDict::new(py);
            for (key, value) in set {
                fields.set_item(key, field_to_python(py, key, value)?)?;
            }
            dict.set_item("set", fields)?;
            dict.set_item("removed", removed)?;
        }
        Change::CatalogueRule { uuid } => {
            dict.set_item("kind", "catalogue_rule")?;
            dict.set_item("uuid", uuid)?;
        }
    }
    Ok(dict.into_any().unbind())
}

#[pyclass(module = "cocat_rs", frozen)]
#[derive(Clone, PartialEq, Eq)]
pub struct ProjectInfo {
    #[pyo3(get)]
    pub id: String,
    #[pyo3(get)]
    pub name: String,
    #[pyo3(get)]
    pub role: String,
    /// The user's own project, made with the account: the one to open by default.
    #[pyo3(get)]
    pub personal: bool,
    /// Whether the project's editors may share it as its owner does.
    #[pyo3(get)]
    pub editors_can_share: bool,
}

#[pymethods]
impl ProjectInfo {
    #[new]
    #[pyo3(signature = (id, name, role, personal = false, editors_can_share = true))]
    fn new(
        id: String,
        name: String,
        role: String,
        personal: bool,
        editors_can_share: bool,
    ) -> ProjectInfo {
        ProjectInfo {
            id,
            name,
            role,
            personal,
            editors_can_share,
        }
    }
}

impl From<ClientRoomInfo> for ProjectInfo {
    fn from(info: ClientRoomInfo) -> ProjectInfo {
        ProjectInfo {
            id: info.id,
            name: info.name,
            role: info.role.as_str().to_owned(),
            personal: info.personal,
            editors_can_share: info.editors_can_share,
        }
    }
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> Result<T> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
    Ok(runtime.block_on(future))
}

#[pyfunction]
fn login(py: Python<'_>, url: String, username: String, password: String) -> Result<String> {
    let cookie = py.detach(|| block_on(cocat_client::login(&url, &username, &password)))?;
    cookie.map_err(server_errors::login_error)
}

/// The id of the account behind the cookie.
#[pyfunction]
fn account_id(py: Python<'_>, url: String, cookie: String) -> Result<String> {
    let id = py.detach(|| block_on(cocat_client::account_id(&url, &cookie)))?;
    id.map_err(http_error)
}

/// The account behind the cookie: `id`, `email` and `is_curator`.
#[pyfunction]
fn account<'py>(py: Python<'py>, url: String, cookie: String) -> Result<Bound<'py, PyDict>> {
    let account = py
        .detach(|| block_on(cocat_client::account(&url, &cookie)))?
        .map_err(http_error)?;
    let dict = PyDict::new(py);
    dict.set_item("id", account.id)?;
    dict.set_item("email", account.email)?;
    dict.set_item("is_curator", account.is_curator)?;
    Ok(dict)
}

#[pyfunction]
fn projects(py: Python<'_>, url: String, cookie: String) -> Result<Vec<ProjectInfo>> {
    let projects = py
        .detach(|| block_on(cocat_client::rooms(&url, &cookie)))?
        .map_err(http_error)?;
    Ok(projects.into_iter().map(ProjectInfo::from).collect())
}

/// The project's versions, newest first, as dicts; with `catalogue`, only those that touched it.
#[pyfunction]
#[pyo3(signature = (url, cookie, project_id, catalogue=None))]
fn history(
    py: Python<'_>,
    url: String,
    cookie: String,
    project_id: String,
    catalogue: Option<String>,
) -> Result<Vec<Py<PyAny>>> {
    let versions = py
        .detach(|| {
            block_on(cocat_client::history(
                &url,
                &cookie,
                &project_id,
                catalogue.as_deref(),
            ))
        })?
        .map_err(http_error)?;
    versions
        .iter()
        .map(|version| to_python(py, version))
        .collect()
}

/// What a version changed as a dict (events and catalogues, each with its change, and the
/// event's fields before and after); with `since`, what changed between the two versions.
#[pyfunction]
#[pyo3(signature = (url, cookie, project_id, version, catalogue=None, since=None))]
fn version_changes(
    py: Python<'_>,
    url: String,
    cookie: String,
    project_id: String,
    version: u64,
    catalogue: Option<String>,
    since: Option<u64>,
) -> Result<Py<PyAny>> {
    let diff = py
        .detach(|| {
            block_on(cocat_client::version_changes(
                &url,
                &cookie,
                &project_id,
                version,
                catalogue.as_deref(),
                since,
            ))
        })?
        .map_err(http_error)?;
    to_python(py, &diff)
}

#[pyfunction]
#[pyo3(signature = (url, cookie, project_id, version, catalogue=None))]
fn restore(
    py: Python<'_>,
    url: String,
    cookie: String,
    project_id: String,
    version: u64,
    catalogue: Option<String>,
) -> Result<bool> {
    py.detach(|| {
        block_on(cocat_client::restore(
            &url,
            &cookie,
            &project_id,
            version,
            catalogue.as_deref(),
        ))
    })?
    .map_err(http_error)
}

#[pyfunction]
fn name_version(
    py: Python<'_>,
    url: String,
    cookie: String,
    project_id: String,
    version: u64,
    name: Option<String>,
) -> Result<()> {
    py.detach(|| {
        block_on(cocat_client::name_version(
            &url,
            &cookie,
            &project_id,
            version,
            name.as_deref(),
        ))
    })?
    .map_err(http_error)
}

/// Saves the project's newest version, with an optional name and note; returns its id.
#[pyfunction]
#[pyo3(signature = (url, cookie, project_id, name=None, message=None))]
fn commit(
    py: Python<'_>,
    url: String,
    cookie: String,
    project_id: String,
    name: Option<String>,
    message: Option<String>,
) -> Result<u64> {
    py.detach(|| {
        block_on(cocat_client::commit(
            &url,
            &cookie,
            &project_id,
            name.as_deref(),
            message.as_deref(),
        ))
    })?
    .map_err(http_error)
}

/// Makes a new project, owned by the caller, holding the version; returns its id.
#[pyfunction]
fn copy_version(
    py: Python<'_>,
    url: String,
    cookie: String,
    project_id: String,
    version: u64,
    name: String,
) -> Result<String> {
    py.detach(|| {
        block_on(cocat_client::copy_version(
            &url,
            &cookie,
            &project_id,
            version,
            &name,
        ))
    })?
    .map_err(http_error)
}

/// The compiled part of `cocat_rs`; `cocat_rs/__init__.py` re-exports it.
/// The sync thread's warnings (a server out of reach, a refused handshake) go to Python's
/// `logging` under `cocat_client.*`. Levels are read from Python on each record so that
/// `setLevel` applies at once; below `Info` nothing crosses, so the GIL is rarely taken.
fn forward_logs_to_python(py: Python<'_>) -> PyResult<()> {
    // An embedding application may have installed its own logger first; it then keeps it.
    let _ = pyo3_log::Logger::new(py, pyo3_log::Caching::Loggers)?
        .filter(log::LevelFilter::Info)
        .install();
    Ok(())
}

/// The rule of a search over all events, for scripts that build rules. A catalogue is named
/// by uuid here: there is no project to look names up in.
#[pyfunction]
fn parse_query(py: Python<'_>, text: &str) -> Result<Py<PyAny>> {
    let predicate = query::parse(text, &[]).map_err(value_error)?;
    to_python(py, &query::rule(&predicate, None))
}

/// The rule a tscat predicate (its `to_dict()` form) becomes.
#[pyfunction]
fn rule_from_tscat(py: Python<'_>, predicate: &Bound<'_, PyAny>) -> Result<Py<PyAny>> {
    let rule = cocat_core::rules::rule_from_tscat(&to_json(predicate)?).map_err(value_error)?;
    to_python(py, &rule)
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    forward_logs_to_python(module.py())?;
    module.add_class::<Project>()?;
    module.add_class::<Catalogue>()?;
    module.add_class::<Event>()?;
    module.add_class::<ProjectInfo>()?;
    module.add_function(wrap_pyfunction!(login, module)?)?;
    module.add_function(wrap_pyfunction!(projects, module)?)?;
    module.add_function(wrap_pyfunction!(account_id, module)?)?;
    module.add_function(wrap_pyfunction!(account, module)?)?;
    module.add_function(wrap_pyfunction!(history, module)?)?;
    module.add_function(wrap_pyfunction!(version_changes, module)?)?;
    module.add_function(wrap_pyfunction!(restore, module)?)?;
    module.add_function(wrap_pyfunction!(name_version, module)?)?;
    module.add_function(wrap_pyfunction!(commit, module)?)?;
    module.add_function(wrap_pyfunction!(copy_version, module)?)?;
    module.add_function(wrap_pyfunction!(export_votable, module)?)?;
    module.add_function(wrap_pyfunction!(tfcat_json, module)?)?;
    module.add_function(wrap_pyfunction!(export_tscat_json, module)?)?;
    module.add_function(wrap_pyfunction!(rule_from_tscat, module)?)?;
    module.add_function(wrap_pyfunction!(parse_query, module)?)?;
    module.add("NotFound", not_found(module.py())?)?;
    server_errors::register(module)?;
    sharing::register(module)?;
    submissions::register(module)?;
    sign_in::register(module)?;
    Ok(())
}
