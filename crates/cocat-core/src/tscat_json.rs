//! tscat's JSON files (`tscat/import_export.py`): read into a project, and written from one.
//!
//! Uuids are kept, so importing again updates what an earlier import wrote. A rating (1 to
//! 10) becomes a confidence (0 to 1) and back. A file may also carry `removed` (the uuids
//! of items in tscat's trash) and `members` (each catalogue's members as tscat filters
//! them): tscat never writes these, the Python import of a tscat database does.

use std::collections::{BTreeSet, HashMap};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use yrs::{ReadTxn, TransactionMut};

use crate::model::{
    self, AttributeType, CatalogueSummary, EventField, EventRecord, NewCatalogue, Origin,
    OriginKind, Status, TypeWarning, WriteError, Written,
};
use crate::rules::rule_from_tscat;
use crate::time::Timestamp;

/// The origin every imported event names.
pub const ORIGIN: &str = "tscat";

/// tscat stores a datetime attribute as orjson's text of it, so its type is lost; a value
/// of exactly that form is taken back as a datetime, which the project registers as a date.
static TSCAT_DATETIME: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,6})?(\+00:00)?$").ok()
});

#[derive(Debug, thiserror::Error)]
pub enum TscatJsonError {
    #[error("Import tscat JSON: the file is not tscat's JSON: {0}")]
    Format(#[from] serde_json::Error),
    #[error("Import tscat JSON: event {0} has a {1} that is not a time: {2:?}")]
    Time(String, &'static str, String),
}

#[derive(Deserialize)]
struct File {
    catalogues: Vec<TscatCatalogue>,
    events: Vec<TscatEvent>,
    #[serde(default)]
    removed: BTreeSet<String>,
    #[serde(default)]
    members: HashMap<String, Vec<String>>,
}

#[derive(Serialize, Deserialize)]
struct TscatEvent {
    start: String,
    stop: String,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    products: Option<Vec<String>>,
    #[serde(default)]
    rating: Option<Number>,
    #[serde(flatten)]
    attributes: Map<String, Value>,
}

#[derive(Serialize, Deserialize)]
struct TscatCatalogue {
    name: String,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    predicate: Option<Value>,
    #[serde(flatten)]
    attributes: Map<String, Value>,
    events: Vec<String>,
}

/// A tscat file read and converted, ready to write into a project.
#[derive(Debug, Clone, PartialEq)]
pub struct TscatImport {
    /// Each event as the project will hold it; one in tscat's trash has the `deleted` status.
    pub events: Vec<EventRecord>,
    pub catalogues: Vec<IncomingCatalogue>,
    /// The attribute keys whose values were datetimes, to register as dates.
    pub dates: BTreeSet<String>,
    /// What could not come across as it was, in words.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IncomingCatalogue {
    pub uuid: String,
    pub name: String,
    pub author: Option<String>,
    pub tags: Vec<String>,
    pub attributes: Map<String, Value>,
    pub rule: Option<Value>,
    /// The events it lists.
    pub events: Vec<String>,
    pub removed: bool,
}

pub fn parse(text: &str) -> Result<TscatImport, TscatJsonError> {
    let File {
        catalogues,
        events,
        removed,
        members,
    } = serde_json::from_str(text)?;
    let mut dates = BTreeSet::new();
    let events = events
        .into_iter()
        .map(|event| event_of(event, &removed, &mut dates))
        .collect::<Result<_, _>>()?;
    let mut warnings = Vec::new();
    let catalogues = catalogues
        .into_iter()
        .map(|catalogue| catalogue_of(catalogue, &removed, &members, &mut warnings))
        .collect();
    Ok(TscatImport {
        events,
        catalogues,
        dates,
        warnings,
    })
}

fn event_of(
    event: TscatEvent,
    removed: &BTreeSet<String>,
    dates: &mut BTreeSet<String>,
) -> Result<EventRecord, TscatJsonError> {
    let uuid = event.uuid.unwrap_or_else(new_uuid);
    let time = |field: &'static str, text: &str| {
        Timestamp::parse(text).ok_or_else(|| TscatJsonError::Time(uuid.clone(), field, text.into()))
    };
    let mut record = EventRecord::new(
        uuid.clone(),
        time("start", &event.start)?,
        time("stop", &event.stop)?,
    );
    record.author = event.author.filter(|author| !author.is_empty());
    record.origin = Some(ORIGIN.to_owned());
    record.tags = event.tags.unwrap_or_default();
    record.products = event.products.unwrap_or_default();
    record.confidence = event
        .rating
        .and_then(|rating| rating.as_f64())
        .map(|rating| rating / 10.0);
    if removed.contains(&uuid) {
        record.status = Status::Deleted;
    }
    for (key, value) in event.attributes {
        let value = attribute_value(value);
        if is_date(&value) {
            dates.insert(key.clone());
        }
        record.attributes.insert(key, value.into_value());
    }
    Ok(record)
}

/// tscat writes a predicate as its Python repr, which says nothing a rule can use: such a
/// catalogue keeps the members tscat wrote, as listed events. A predicate dict that does not
/// convert leaves the catalogue static, listing every member tscat gives it now.
fn catalogue_of(
    catalogue: TscatCatalogue,
    removed: &BTreeSet<String>,
    members: &HashMap<String, Vec<String>>,
    warnings: &mut Vec<String>,
) -> IncomingCatalogue {
    let uuid = catalogue.uuid.unwrap_or_else(new_uuid);
    let name = catalogue.name;
    let (rule, events) = match catalogue.predicate {
        None | Some(Value::Null) => (None, catalogue.events),
        Some(Value::String(_)) => {
            warnings.push(format!(
                "catalogue {name}: tscat wrote its predicate as text, \
                 so it is imported with its members and no rule"
            ));
            (None, catalogue.events)
        }
        Some(predicate) => match rule_from_tscat(&predicate) {
            Ok(rule) => (Some(rule), catalogue.events),
            Err(error) => {
                warnings.push(format!(
                    "catalogue {name}: its predicate does not convert ({error}), \
                     so it is imported with its current members and no rule"
                ));
                (
                    None,
                    members.get(&uuid).cloned().unwrap_or(catalogue.events),
                )
            }
        },
    };
    IncomingCatalogue {
        removed: removed.contains(&uuid),
        uuid,
        name,
        author: catalogue.author.filter(|author| !author.is_empty()),
        tags: catalogue.tags.unwrap_or_default(),
        attributes: catalogue
            .attributes
            .into_iter()
            .map(|(key, value)| (key, attribute_value(value).into_value()))
            .collect(),
        rule,
        events,
    }
}

/// An attribute value, with tscat's datetime text read back as a time.
enum Attribute {
    Date(Timestamp),
    Other(Value),
}

impl Attribute {
    fn into_value(self) -> Value {
        match self {
            Attribute::Date(time) => Value::from(time.to_string()),
            Attribute::Other(value) => value,
        }
    }
}

fn attribute_value(value: Value) -> Attribute {
    let date = match &value {
        Value::String(text) if is_tscat_datetime(text) => Timestamp::parse(text),
        _ => None,
    };
    date.map_or(Attribute::Other(value), Attribute::Date)
}

fn is_date(value: &Attribute) -> bool {
    matches!(value, Attribute::Date(_))
}

fn is_tscat_datetime(text: &str) -> bool {
    TSCAT_DATETIME
        .as_ref()
        .is_some_and(|pattern| pattern.is_match(text))
}

fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Writes the import in the caller's transaction. An event or a catalogue the project
/// already holds is updated: tscat's fields are set, the fields tscat does not know (status,
/// comment, errors) stay. Only what is in the trash is restored, so a status set in the
/// project stays. `now` is the new catalogues' creation time.
pub fn write(
    txn: &mut TransactionMut<'_>,
    import: &TscatImport,
    now: Timestamp,
) -> Result<Written, WriteError> {
    ensure_origin(txn)?;
    for key in &import.dates {
        model::register_attribute(txn, key, AttributeType::Date);
    }
    let mut written = Written::default();
    for event in &import.events {
        written.warnings.extend(put_event(txn, event)?);
    }
    for catalogue in &import.catalogues {
        put_catalogue(txn, catalogue, now)?;
        written.catalogues.push(catalogue.uuid.clone());
    }
    Ok(written)
}

fn ensure_origin(txn: &mut TransactionMut<'_>) -> Result<(), WriteError> {
    if model::origin(txn, ORIGIN).is_some() {
        return Ok(());
    }
    model::create_origin(
        txn,
        &Origin {
            id: ORIGIN.to_owned(),
            kind: OriginKind::Import,
            name: "tscat".to_owned(),
            version: None,
            author: None,
            created: None,
            used: Vec::new(),
            parameters: None,
            derived_from: None,
        },
    )
}

fn put_event(
    txn: &mut TransactionMut<'_>,
    event: &EventRecord,
) -> Result<Vec<TypeWarning>, WriteError> {
    let Some(existing) = model::event(txn, &event.uuid) else {
        return model::create_event(txn, event);
    };
    let uuid = event.uuid.as_str();
    model::set_event_range(txn, uuid, event.start, event.stop)?;
    let fields = [
        EventField::Author(event.author.clone()),
        EventField::Tags(event.tags.clone()),
        EventField::Products(event.products.clone()),
        EventField::Confidence(event.confidence),
    ];
    for field in &fields {
        model::set_event_field(txn, uuid, field)?;
    }
    let stale = stale_keys(&existing.attributes, &event.attributes);
    model::remove_event_attributes(txn, uuid, &stale);
    let warnings = model::set_event_attributes(txn, uuid, &event.attributes)?.unwrap_or_default();
    if event.status == Status::Deleted {
        model::delete_event(txn, uuid);
    } else if existing.status == Status::Deleted {
        model::restore_event(txn, uuid);
    }
    Ok(warnings)
}

fn put_catalogue(
    txn: &mut TransactionMut<'_>,
    catalogue: &IncomingCatalogue,
    now: Timestamp,
) -> Result<(), WriteError> {
    let uuid = catalogue.uuid.as_str();
    let in_trash = match model::catalogue(txn, uuid) {
        Some(existing) => {
            update_catalogue(txn, catalogue, &existing)?;
            existing.deleted
        }
        None => {
            model::create_catalogue(
                txn,
                &NewCatalogue {
                    uuid: catalogue.uuid.clone(),
                    name: catalogue.name.clone(),
                    author: catalogue.author.clone(),
                    created: Some(now),
                    tags: catalogue.tags.clone(),
                    columns: Vec::new(),
                    attributes: catalogue.attributes.clone(),
                },
            )?;
            false
        }
    };
    model::add_catalogue_events(txn, uuid, &catalogue.events);
    model::set_catalogue_rule(txn, uuid, catalogue.rule.as_ref())?;
    if catalogue.removed {
        model::delete_catalogue(txn, uuid);
    } else if in_trash {
        model::restore_catalogue(txn, uuid);
    }
    Ok(())
}

/// The name, tags, attributes and listed events become the file's; the author stays.
fn update_catalogue(
    txn: &mut TransactionMut<'_>,
    catalogue: &IncomingCatalogue,
    existing: &CatalogueSummary,
) -> Result<(), WriteError> {
    let uuid = catalogue.uuid.as_str();
    model::rename_catalogue(txn, uuid, &catalogue.name);
    model::set_catalogue_tags(txn, uuid, &catalogue.tags);
    let stale = stale_keys(&existing.attributes, &catalogue.attributes);
    model::remove_catalogue_attributes(txn, uuid, &stale);
    model::set_catalogue_attributes(txn, uuid, &catalogue.attributes)?;
    let keep: BTreeSet<&String> = catalogue.events.iter().collect();
    let dropped: Vec<String> = model::listed_events(txn, uuid)
        .into_iter()
        .filter(|event| !keep.contains(event))
        .collect();
    model::remove_catalogue_events(txn, uuid, &dropped);
    Ok(())
}

fn stale_keys(old: &Map<String, Value>, new: &Map<String, Value>) -> Vec<String> {
    old.keys()
        .filter(|key| !new.contains_key(*key))
        .cloned()
        .collect()
}

/// The catalogues and their member events in tscat's JSON. A smart catalogue is written
/// with its current members as events and no predicate; derived intervals are left out,
/// having no uuid. An event in several catalogues is written once.
pub fn export<T: ReadTxn>(
    txn: &T,
    catalogues: &[(&CatalogueSummary, &[EventRecord])],
) -> Result<String, serde_json::Error> {
    let dates: BTreeSet<String> = model::attribute_types(txn)
        .into_iter()
        .filter(|(_, info)| info.kind == AttributeType::Date)
        .map(|(key, _)| key)
        .collect();
    let mut seen = BTreeSet::new();
    let events: Vec<TscatEvent> = catalogues
        .iter()
        .flat_map(|(_, events)| events.iter())
        .filter(|event| seen.insert(event.uuid.as_str()))
        .map(|event| event_to_tscat(event, &dates))
        .collect();
    let catalogues: Vec<TscatCatalogue> = catalogues
        .iter()
        .map(|(catalogue, events)| catalogue_to_tscat(catalogue, events))
        .collect();
    serde_json::to_string(&serde_json::json!({
        "catalogues": catalogues,
        "events": events,
    }))
}

fn event_to_tscat(event: &EventRecord, dates: &BTreeSet<String>) -> TscatEvent {
    let attributes = event
        .attributes
        .iter()
        .map(|(key, value)| {
            let date = match value {
                Value::String(text) if dates.contains(key) => Timestamp::parse(text),
                _ => None,
            };
            let value = date.map_or_else(|| value.clone(), |date| Value::from(isoformat(date)));
            (key.clone(), value)
        })
        .collect();
    TscatEvent {
        start: naive_isoformat(event.start),
        stop: naive_isoformat(event.stop),
        author: Some(event.author.clone().unwrap_or_default()),
        uuid: Some(event.uuid.clone()),
        tags: Some(sorted(&event.tags)),
        products: Some(sorted(&event.products)),
        rating: event.confidence.map(rating),
        attributes,
    }
}

fn catalogue_to_tscat(catalogue: &CatalogueSummary, events: &[EventRecord]) -> TscatCatalogue {
    let mut uuids: Vec<String> = events.iter().map(|event| event.uuid.clone()).collect();
    uuids.sort();
    TscatCatalogue {
        name: catalogue.name.clone(),
        author: Some(catalogue.author.clone().unwrap_or_default()),
        uuid: Some(catalogue.uuid.clone()),
        tags: Some(sorted(&catalogue.tags)),
        predicate: Some(Value::Null),
        attributes: catalogue.attributes.clone(),
        events: uuids,
    }
}

fn sorted(items: &[String]) -> Vec<String> {
    let mut items = items.to_vec();
    items.sort();
    items
}

/// tscat takes a whole rating from 1 to 10. Python's `round` rounds a half to even.
fn rating(confidence: f64) -> Number {
    Number::from((confidence * 10.0).round_ties_even().clamp(1.0, 10.0) as i64)
}

/// Python's `datetime.isoformat()` of the aware UTC datetime.
fn isoformat(time: Timestamp) -> String {
    time.as_text()
        .map(|text| text.replacen(' ', "T", 1))
        .unwrap_or_else(|_| time.to_string())
}

/// tscat writes naive UTC times.
fn naive_isoformat(time: Timestamp) -> String {
    let text = isoformat(time);
    text.strip_suffix("+00:00")
        .map(str::to_owned)
        .unwrap_or(text)
}

#[cfg(test)]
mod tests;
