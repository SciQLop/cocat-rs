//! The compiled part of `cocat_rs`: a room, its catalogues and its events.
//!
//! Everything that can be done without Python types happens in Rust; the Python wrapper
//! (`cocat_rs/__init__.py`) adds the asyncio and listener plumbing that needs a loop.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use cocat_client::{Change, Credentials, Room as ClientRoom, RoomInfo as ClientRoomInfo};
use cocat_core::model::{self, NewCatalogue, NewEvent};
use cocat_core::time::Timestamp;
use cocat_core::votable;
use numpy::PyArray1;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDateAccess, PyDateTime, PyDict, PyList, PySet, PyTimeAccess, PyTzInfo};
use serde_json::{Map as JsonMap, Value};
use yrs::Transact;

type Result<T> = PyResult<T>;

/// `save()` waits for the server's acknowledgement this long before it gives up.
const DEFAULT_SAVE_TIMEOUT: Duration = Duration::from_secs(30);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
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

fn to_json_map(dict: &Bound<'_, PyDict>) -> Result<JsonMap<String, Value>> {
    let mut map = JsonMap::new();
    for (key, value) in dict.iter() {
        map.insert(key.extract::<String>()?, to_json(&value)?);
    }
    Ok(map)
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
    }
}

fn unknown(what: &str) -> PyErr {
    PyRuntimeError::new_err(format!("no {what} with that uuid"))
}

/// Whether two reads of one event hold the same values, whatever room each was read from.
fn same_event(a: &model::EventRecord, b: &model::EventRecord) -> bool {
    a.uuid == b.uuid
        && a.start == b.start
        && a.stop == b.stop
        && a.author == b.author
        && a.rating == b.rating
        && a.attributes == b.attributes
        && sorted(&a.tags) == sorted(&b.tags)
        && sorted(&a.products) == sorted(&b.products)
}

/// Whether two reads of one catalogue hold the same values. Its member events are not
/// compared: two clients agree on a catalogue's fields before they agree on its contents.
fn same_catalogue(a: &model::CatalogueSummary, b: &model::CatalogueSummary) -> bool {
    a.uuid == b.uuid
        && a.name == b.name
        && a.author == b.author
        && a.attributes == b.attributes
        && sorted(&a.tags) == sorted(&b.tags)
}

fn sorted(values: &[String]) -> Vec<&str> {
    let mut sorted: Vec<&str> = values.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted
}

#[pyclass(module = "cocat_rs", frozen)]
#[derive(Clone)]
pub struct Catalogue {
    room: Arc<ClientRoom>,
    uuid: String,
}

impl Catalogue {
    fn of(room: &Arc<ClientRoom>, uuid: &str) -> Catalogue {
        Catalogue {
            room: Arc::clone(room),
            uuid: uuid.to_owned(),
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
        let txn = self.room.doc().transact();
        model::catalogue(&txn, &self.uuid)
            .map(|catalogue| catalogue.name)
            .ok_or_else(|| unknown("catalogue"))
    }

    #[setter]
    fn set_name(&self, name: String) {
        self.room
            .write(|txn| model::rename_catalogue(txn, &self.uuid, &name));
    }

    #[getter]
    fn author(&self) -> Result<String> {
        let txn = self.room.doc().transact();
        model::catalogue(&txn, &self.uuid)
            .map(|catalogue| catalogue.author)
            .ok_or_else(|| unknown("catalogue"))
    }

    #[getter]
    fn tags(&self, py: Python<'_>) -> Result<Py<PyAny>> {
        let txn = self.room.doc().transact();
        let catalogue = model::catalogue(&txn, &self.uuid).ok_or_else(|| unknown("catalogue"))?;
        Ok(PySet::new(py, catalogue.tags)?.into_any().unbind())
    }

    #[getter]
    fn attributes(&self, py: Python<'_>) -> Result<Py<PyAny>> {
        let txn = self.room.doc().transact();
        let catalogue = model::catalogue(&txn, &self.uuid).ok_or_else(|| unknown("catalogue"))?;
        to_python_map(py, &catalogue.attributes)
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
        self.room
            .write(|txn| model::set_catalogue_attributes(txn, &self.uuid, &attributes))
            .ok()
            .ok_or_else(|| unknown("catalogue"))?;
        Ok(())
    }

    fn remove_attributes(&self, keys: &Bound<'_, PyAny>) -> Result<()> {
        let keys = uuids(Some(keys))?;
        if !self
            .room
            .write(|txn| model::remove_catalogue_attributes(txn, &self.uuid, &keys))
        {
            return Err(unknown("catalogue"));
        }
        Ok(())
    }

    fn add_events(&self, events: &Bound<'_, PyAny>) -> Result<()> {
        let events = uuids(Some(events))?;
        if !self
            .room
            .write(|txn| model::add_catalogue_events(txn, &self.uuid, &events))
        {
            return Err(unknown("catalogue"));
        }
        Ok(())
    }

    fn remove_events(&self, events: &Bound<'_, PyAny>) -> Result<()> {
        let events = uuids(Some(events))?;
        if !self
            .room
            .write(|txn| model::remove_catalogue_events(txn, &self.uuid, &events))
        {
            return Err(unknown("catalogue"));
        }
        Ok(())
    }

    fn delete(&self) {
        self.room
            .write(|txn| model::delete_catalogue(txn, &self.uuid));
    }

    /// Uuids, start and stop times, and with `attributes=True` each event's attributes, all
    /// from one read of the document, so a remote change cannot fall between them.
    #[pyo3(signature = (attributes=false))]
    fn event_arrays(&self, py: Python<'_>, attributes: bool) -> Result<Py<PyAny>> {
        event_arrays_of(py, &self.records()?, attributes)
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
        for record in self.records()? {
            let map: JsonMap<String, Value> = match &wanted {
                Some(keys) => record
                    .attributes
                    .iter()
                    .filter(|(key, _)| keys.contains(key))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
                None => record.attributes.clone(),
            };
            rows.append(to_python_map(py, &map)?)?;
        }
        Ok(rows.into_any().unbind())
    }
}

impl Catalogue {
    fn summary(&self) -> Result<model::CatalogueSummary> {
        let txn = self.room.doc().transact();
        model::catalogue(&txn, &self.uuid).ok_or_else(|| unknown("catalogue"))
    }

    fn records(&self) -> Result<Vec<model::EventRecord>> {
        let txn = self.room.doc().transact();
        model::catalogue_events(&txn, &self.uuid).ok_or_else(|| unknown("catalogue"))
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

    fn record(&self) -> Result<model::EventRecord> {
        let txn = self.room.doc().transact();
        model::event(&txn, &self.uuid).ok_or_else(|| unknown("event"))
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
        Ok(same_event(&self.record()?, &other.record()?))
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
        self.room
            .write(|txn| model::set_event_range(txn, &self.uuid, start, stop))
            .ok()
            .ok_or_else(|| unknown("event"))?;
        Ok(())
    }

    #[getter]
    fn author(&self) -> Result<String> {
        Ok(self.record()?.author)
    }

    #[getter]
    fn rating(&self) -> Result<Option<i64>> {
        Ok(self.record()?.rating)
    }

    #[getter]
    fn tags(&self, py: Python<'_>) -> Result<Py<PyAny>> {
        Ok(PySet::new(py, self.record()?.tags)?.into_any().unbind())
    }

    #[getter]
    fn products(&self, py: Python<'_>) -> Result<Py<PyAny>> {
        Ok(PySet::new(py, self.record()?.products)?.into_any().unbind())
    }

    #[getter]
    fn attributes(&self, py: Python<'_>) -> Result<Py<PyAny>> {
        to_python_map(py, &self.record()?.attributes)
    }

    /// The uuids of the catalogues that list this event, sorted.
    #[getter]
    fn catalogues(&self, py: Python<'_>) -> Result<Py<PyAny>> {
        let txn = self.room.doc().transact();
        let uuids = model::event_catalogues(&txn, &self.uuid);
        Ok(PyList::new(py, uuids)?.into_any().unbind())
    }

    #[pyo3(signature = (**attributes))]
    fn set_attributes(&self, attributes: Option<&Bound<'_, PyDict>>) -> Result<()> {
        let Some(attributes) = attributes else {
            return Ok(());
        };
        let attributes = to_json_map(attributes)?;
        self.room
            .write(|txn| model::set_event_attributes(txn, &self.uuid, &attributes))
            .ok()
            .ok_or_else(|| unknown("event"))?;
        Ok(())
    }

    fn remove_attributes(&self, keys: &Bound<'_, PyAny>) -> Result<()> {
        let keys = uuids(Some(keys))?;
        if !self
            .room
            .write(|txn| model::remove_event_attributes(txn, &self.uuid, &keys))
        {
            return Err(unknown("event"));
        }
        Ok(())
    }

    fn delete(&self) {
        self.room.write(|txn| model::delete_event(txn, &self.uuid));
    }
}

/// The arrays Python scripts plot with, in the order the records come.
fn event_arrays_of(
    py: Python<'_>,
    records: &[model::EventRecord],
    attributes: bool,
) -> Result<Py<PyAny>> {
    let uuids: Vec<String> = records.iter().map(|record| record.uuid.clone()).collect();
    let starts: Vec<i64> = records
        .iter()
        .map(|record| record.start.unix_micros() as i64)
        .collect();
    let stops: Vec<i64> = records
        .iter()
        .map(|record| record.stop.unix_micros() as i64)
        .collect();
    let arrays = PyDict::new(py);
    arrays.set_item("uuid", uuids)?;
    arrays.set_item("start", datetime_array(py, starts)?)?;
    arrays.set_item("stop", datetime_array(py, stops)?)?;
    if attributes {
        let rows = PyList::empty(py);
        for record in records {
            rows.append(to_python_map(py, &record.attributes)?)?;
        }
        arrays.set_item("attributes", rows)?;
    }
    Ok(arrays.into_any().unbind())
}

fn datetime_array(py: Python<'_>, micros: Vec<i64>) -> Result<Py<PyAny>> {
    let array = PyArray1::from_vec(py, micros);
    Ok(array.call_method1("view", ("datetime64[us]",))?.unbind())
}

#[pyclass(module = "cocat_rs")]
pub struct Room {
    room: Arc<ClientRoom>,
    batches: Mutex<Receiver<Vec<Change>>>,
    url: Mutex<Option<String>>,
    credentials: Mutex<Option<(String, String)>>,
}

#[pymethods]
impl Room {
    #[staticmethod]
    fn open(cache_dir: String, room_id: String) -> Result<Room> {
        std::fs::create_dir_all(Path::new(&cache_dir))
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        let room = Arc::new(
            ClientRoom::open(Path::new(&cache_dir), &room_id)
                .map_err(|error| PyValueError::new_err(error.to_string()))?,
        );
        let (sender, batches) = mpsc::channel();
        room.subscribe(move |batch| {
            let _ = sender.send(batch);
        });
        Ok(Room {
            room,
            batches: Mutex::new(batches),
            url: Mutex::new(None),
            credentials: Mutex::new(None),
        })
    }

    /// Connects, and keeps `(username, password)` so a session the server refuses can log in
    /// again; `set_cookie` reuses them.
    #[pyo3(signature = (url, cookie, credentials=None))]
    fn connect(
        &self,
        url: String,
        cookie: String,
        credentials: Option<(String, String)>,
    ) -> Result<()> {
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
            return Err(PyRuntimeError::new_err("this room never connected"));
        };
        let login = lock(&self.credentials)
            .clone()
            .map(|(username, password)| Credentials::new(username, password));
        self.room
            .connect_with(&url, &cookie, login)
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))
    }

    fn close(&self) {
        self.room.close();
    }

    #[getter]
    fn status(&self) -> &'static str {
        status_name(self.room.status())
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
        let txn = self.room.doc().transact();
        model::catalogues(&txn)
            .iter()
            .map(|catalogue| Py::new(py, Catalogue::of(&self.room, &catalogue.uuid)))
            .collect()
    }

    fn catalogue(&self, py: Python<'_>, uuid_or_name: &str) -> Result<Py<Catalogue>> {
        let txn = self.room.doc().transact();
        let found = model::catalogue(&txn, uuid_or_name).or_else(|| {
            model::catalogues(&txn)
                .into_iter()
                .find(|catalogue| catalogue.name == uuid_or_name)
        });
        let catalogue = found.ok_or_else(|| unknown("catalogue"))?;
        Py::new(py, Catalogue::of(&self.room, &catalogue.uuid))
    }

    fn event(&self, py: Python<'_>, uuid: &str) -> Result<Py<Event>> {
        let txn = self.room.doc().transact();
        model::event(&txn, uuid).ok_or_else(|| unknown("event"))?;
        Py::new(py, Event::of(&self.room, uuid))
    }

    /// The arrays of the events named (uuids or `Event`s), in the order given and from one
    /// read of the document. A uuid the room does not hold is left out, and the returned
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
            let txn = self.room.doc().transact();
            wanted
                .iter()
                .filter_map(|uuid| model::event(&txn, uuid))
                .collect::<Vec<_>>()
        };
        event_arrays_of(py, &records, attributes)
    }

    #[pyo3(signature = (name, author, uuid=None, tags=None, attributes=None, events=None))]
    #[allow(clippy::too_many_arguments)]
    fn create_catalogue(
        &self,
        py: Python<'_>,
        name: String,
        author: String,
        uuid: Option<String>,
        tags: Option<&Bound<'_, PyAny>>,
        attributes: Option<&Bound<'_, PyDict>>,
        events: Option<&Bound<'_, PyAny>>,
    ) -> Result<Py<Catalogue>> {
        let uuid = uuid_argument(uuid)?;
        let tags = uuids(tags)?;
        let attributes = match attributes {
            Some(attributes) => to_json_map(attributes)?,
            None => JsonMap::new(),
        };
        let catalogue = NewCatalogue {
            uuid: &uuid,
            name: &name,
            author: &author,
            tags: &tags,
            attributes: &attributes,
        };
        self.room
            .write(|txn| model::create_catalogue(txn, &catalogue))
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        if let Some(events) = events {
            let mut uuids = Vec::new();
            for event in events.try_iter()? {
                uuids.push(event_uuid(&event?)?);
            }
            self.room
                .write(|txn| model::add_catalogue_events(txn, &uuid, &uuids));
        }
        Py::new(py, Catalogue::of(&self.room, &uuid))
    }

    #[pyo3(signature = (start, stop, author, uuid=None, tags=None, products=None, rating=None, attributes=None))]
    #[allow(clippy::too_many_arguments)]
    fn create_event(
        &self,
        py: Python<'_>,
        start: &Bound<'_, PyAny>,
        stop: &Bound<'_, PyAny>,
        author: String,
        uuid: Option<String>,
        tags: Option<&Bound<'_, PyAny>>,
        products: Option<&Bound<'_, PyAny>>,
        rating: Option<i64>,
        attributes: Option<&Bound<'_, PyDict>>,
    ) -> Result<Py<Event>> {
        let uuid = uuid_argument(uuid)?;
        let start = to_timestamp(start)?;
        let stop = to_timestamp(stop)?;
        let tags = uuids(tags)?;
        let products = uuids(products)?;
        let attributes = match attributes {
            Some(attributes) => to_json_map(attributes)?,
            None => JsonMap::new(),
        };
        let event = NewEvent {
            uuid: &uuid,
            start,
            stop,
            author: &author,
            tags: &tags,
            products: &products,
            rating,
            attributes: &attributes,
        };
        self.room
            .write(|txn| model::create_event(txn, &event))
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        Py::new(py, Event::of(&self.room, &uuid))
    }

    /// Blocks until the server has processed every change made so far, or `timeout` passes.
    #[pyo3(signature = (timeout=None))]
    fn save(&self, py: Python<'_>, timeout: Option<f64>) -> Result<()> {
        let timeout = timeout.map_or(DEFAULT_SAVE_TIMEOUT, Duration::from_secs_f64);
        py.detach(|| self.room.save(timeout))
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))
    }

    /// Imports the VOTable's tables into this room and returns how many catalogues it added.
    #[pyo3(signature = (path, table_name=None))]
    fn import_votable(&self, path: String, table_name: Option<String>) -> Result<usize> {
        let xml = std::fs::read_to_string(&path)
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        let imported =
            votable::import(&xml).map_err(|error| PyValueError::new_err(error.to_string()))?;
        let base = table_name
            .or(imported.name)
            .unwrap_or_else(|| "Imported Catalogue".to_owned());
        let author = imported
            .author
            .unwrap_or_else(|| "VOTable Import".to_owned());
        let single = imported.tables.len() == 1;
        self.room.write(|txn| -> Result<()> {
            for (index, table) in imported.tables.iter().enumerate() {
                let name = if single {
                    base.clone()
                } else {
                    format!("{base}_{index}")
                };
                import_table(txn, &name, &author, &table.events)?;
            }
            Ok(())
        })?;
        Ok(imported.tables.len())
    }
}

/// Writes one imported table as a catalogue holding its events.
fn import_table(
    txn: &mut yrs::TransactionMut<'_>,
    name: &str,
    author: &str,
    events: &[votable::ImportedEvent],
) -> Result<()> {
    let catalogue = uuid::Uuid::new_v4().to_string();
    let mut uuids = Vec::with_capacity(events.len());
    for event in events {
        let record = NewEvent {
            uuid: &event.uuid,
            start: event.start,
            stop: event.stop,
            author: &event.author,
            tags: &event.tags,
            products: &event.products,
            rating: event.rating,
            attributes: &event.attributes,
        };
        model::create_event(txn, &record)
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        uuids.push(event.uuid.clone());
    }
    let new = NewCatalogue {
        uuid: &catalogue,
        name,
        author,
        tags: &[],
        attributes: &JsonMap::new(),
    };
    model::create_catalogue(txn, &new).map_err(|error| PyValueError::new_err(error.to_string()))?;
    model::add_catalogue_events(txn, &catalogue, &uuids);
    Ok(())
}

/// The VOTable a set of catalogues exports to, the file Python cocat's exporter writes.
///
/// The catalogues carry their room, so the script mode can export a room it never
/// logged into.
#[pyfunction]
fn export_votable(catalogues: &Bound<'_, PyAny>) -> Result<String> {
    let (room, uuids) = catalogues_of(catalogues)?;
    let txn = room.doc().transact();
    let mut exported = Vec::new();
    for uuid in &uuids {
        let catalogue = model::catalogue(&txn, uuid).ok_or_else(|| unknown("catalogue"))?;
        let events = model::catalogue_events(&txn, uuid).unwrap_or_default();
        exported.push((catalogue, events));
    }
    let pairs: Vec<(&model::CatalogueSummary, &[model::EventRecord])> = exported
        .iter()
        .map(|(catalogue, events)| (catalogue, events.as_slice()))
        .collect();
    votable::export_many(&pairs).map_err(|error| PyValueError::new_err(error.to_string()))
}

/// The uuids of the catalogues an argument names (one catalogue or an iterable of them),
/// with the room they belong to.
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
    }
    Ok(dict.into_any().unbind())
}

#[pyclass(module = "cocat_rs", frozen)]
#[derive(Clone, PartialEq, Eq)]
pub struct RoomInfo {
    #[pyo3(get)]
    pub id: String,
    #[pyo3(get)]
    pub name: String,
    #[pyo3(get)]
    pub role: String,
}

#[pymethods]
impl RoomInfo {
    #[new]
    fn new(id: String, name: String, role: String) -> RoomInfo {
        RoomInfo { id, name, role }
    }
}

impl From<ClientRoomInfo> for RoomInfo {
    fn from(info: ClientRoomInfo) -> RoomInfo {
        RoomInfo {
            id: info.id,
            name: info.name,
            role: info.role.as_str().to_owned(),
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
    cookie.map_err(|error| PyValueError::new_err(error.to_string()))
}

#[pyfunction]
fn rooms(py: Python<'_>, url: String, cookie: String) -> Result<Vec<RoomInfo>> {
    let rooms = py
        .detach(|| block_on(cocat_client::rooms(&url, &cookie)))?
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    Ok(rooms.into_iter().map(RoomInfo::from).collect())
}

/// The compiled part of `cocat_rs`; `cocat_rs/__init__.py` re-exports it.
#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Room>()?;
    module.add_class::<Catalogue>()?;
    module.add_class::<Event>()?;
    module.add_class::<RoomInfo>()?;
    module.add_function(wrap_pyfunction!(login, module)?)?;
    module.add_function(wrap_pyfunction!(rooms, module)?)?;
    module.add_function(wrap_pyfunction!(export_votable, module)?)?;
    Ok(())
}
