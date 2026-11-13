//! The room layout (v2): the only module that knows how a room document is laid out.
//!
//! An event is a map keyed by its uuid under `events`. It always holds `start` and `stop`
//! (nanoseconds since 1970 UTC) and stores any other field only when it differs from its
//! default; attributes are flat `#key` entries. A catalogue lists its events in a map used
//! as a set, so concurrent additions all win. `origins`, `comments` and the attribute
//! registry are roots of their own, and `meta.layout` says which layout the room holds.
//!
//! Reading is pure: every function borrows a read transaction and returns owned values.
//! Writing happens in the caller's transaction.

mod copies;
mod links;
mod materialize;
mod members;
mod migrate;
mod provenance;
mod read;
mod registry;
mod relay;
mod restore;
mod search;
mod submission;
mod values;
mod write;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::time::Timestamp;

pub use copies::{Hop, copy_chain, set_copied_from};
pub use links::{LINKS, Link, LinkError, NewLink, add_link, links, remove_link};
pub use materialize::materialize;
pub use members::{
    Member, catalogue_events, catalogue_events_at, catalogue_members, catalogue_members_at,
};
pub use migrate::{Migration, migrate_v1};
pub use provenance::{comment, create_origin, origin, set_comment};
pub use read::{
    all_catalogues, catalogue, catalogue_names, catalogues, event, event_catalogues, existing_tags,
    is_v1, listed_events, live_event_count, trash,
};
pub use registry::{attribute_types, register_attribute, set_attribute_info};
pub use relay::{Counts, Wanted, hold};
pub use restore::restore;
pub use search::{Search, record_matches, search};
pub use submission::{adopt, extract};
pub use write::{
    add_catalogue_events, create_catalogue, create_event, delete_catalogue, delete_event,
    purge_catalogue, purge_event, remove_catalogue_attributes, remove_catalogue_events,
    remove_event_attributes, rename_catalogue, restore_catalogue, restore_event,
    set_catalogue_attributes, set_catalogue_columns, set_catalogue_rule, set_catalogue_shape_axis,
    set_catalogue_tags, set_event_attributes, set_event_field, set_event_range,
};

pub const CATALOGUES: &str = "catalogues";
pub const EVENTS: &str = "events";
pub const ORIGINS: &str = "origins";
pub const COMMENTS: &str = "comments";
pub const REGISTRY: &str = "attributes";
pub const META: &str = "meta";
pub const LAYOUT: i64 = 2;
/// The prefix that marks an attribute among an event's or a catalogue's own fields.
pub const ATTRIBUTE_PREFIX: char = '#';

/// QuakeML's evaluation status, plus `deleted` for the trash. A room stores only the
/// statuses other than `confirmed`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Preliminary,
    #[default]
    Confirmed,
    Reviewed,
    Final,
    Rejected,
    Deleted,
}

impl Status {
    pub const ALL: [Status; 6] = [
        Status::Preliminary,
        Status::Confirmed,
        Status::Reviewed,
        Status::Final,
        Status::Rejected,
        Status::Deleted,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Status::Preliminary => "preliminary",
            Status::Confirmed => "confirmed",
            Status::Reviewed => "reviewed",
            Status::Final => "final",
            Status::Rejected => "rejected",
            Status::Deleted => "deleted",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.name() == name)
    }
}

/// One corner of a shape: a time, and a value on the catalogue's shape axis (a frequency, an
/// energy, a pitch angle…).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Vertex {
    #[schema(value_type = String)]
    pub time: Timestamp,
    pub value: f64,
}

/// What the second coordinate of a catalogue's shapes measures, and in which unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ShapeAxis {
    pub quantity: String,
    pub unit: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EventRecord {
    pub uuid: String,
    #[schema(value_type = String)]
    pub start: Timestamp,
    #[schema(value_type = String)]
    pub stop: Timestamp,
    pub author: Option<String>,
    #[schema(value_type = Option<String>)]
    pub created: Option<Timestamp>,
    pub origin: Option<String>,
    #[serde(default)]
    pub status: Status,
    pub confidence: Option<f64>,
    pub start_err: Option<f64>,
    pub stop_err: Option<f64>,
    pub shape: Option<Vec<Vertex>>,
    pub source_id: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub products: Vec<String>,
    #[schema(value_type = Object)]
    #[serde(default)]
    pub attributes: Map<String, Value>,
}

impl EventRecord {
    /// An event made by hand that holds nothing but its range.
    pub fn new(uuid: impl Into<String>, start: Timestamp, stop: Timestamp) -> Self {
        Self {
            uuid: uuid.into(),
            start,
            stop,
            author: None,
            created: None,
            origin: None,
            status: Status::Confirmed,
            confidence: None,
            start_err: None,
            stop_err: None,
            shape: None,
            source_id: None,
            tags: Vec::new(),
            products: Vec::new(),
            attributes: Map::new(),
        }
    }
}

/// One fixed field of an event other than its range, with the value to write; `None` and
/// an empty list remove the field.
#[derive(Debug, Clone, PartialEq)]
pub enum EventField {
    Author(Option<String>),
    Created(Option<Timestamp>),
    Origin(Option<String>),
    Status(Status),
    Confidence(Option<f64>),
    StartErr(Option<f64>),
    StopErr(Option<f64>),
    /// Writing a shape also sets the range to the shape's time span.
    Shape(Option<Vec<Vertex>>),
    SourceId(Option<String>),
    Tags(Vec<String>),
    Products(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CatalogueSummary {
    pub uuid: String,
    pub name: String,
    pub author: Option<String>,
    #[schema(value_type = Option<String>)]
    pub created: Option<Timestamp>,
    pub deleted: bool,
    pub tags: Vec<String>,
    pub columns: Vec<String>,
    #[schema(value_type = Object)]
    pub attributes: Map<String, Value>,
    #[schema(value_type = Option<Object>)]
    pub rule: Option<Value>,
    pub shape_axis: Option<ShapeAxis>,
    pub copied_from: Vec<Hop>,
    pub event_count: usize,
}

/// The fields a catalogue is created from. The uuid is the caller's.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewCatalogue {
    pub uuid: String,
    pub name: String,
    pub author: Option<String>,
    pub created: Option<Timestamp>,
    pub tags: Vec<String>,
    pub columns: Vec<String>,
    pub attributes: Map<String, Value>,
}

/// W3C PROV's kinds of activity an event can be generated by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OriginKind {
    Rule,
    Detector,
    Model,
    Import,
}

/// A data product an origin used, with the version or the date it was retrieved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Used {
    pub product: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieved: Option<String>,
}

/// What made a set of events: everything that is the same for a whole run.
#[derive(Debug, Clone, PartialEq)]
pub struct Origin {
    pub id: String,
    pub kind: OriginKind,
    pub name: String,
    pub version: Option<String>,
    pub author: Option<String>,
    pub created: Option<Timestamp>,
    pub used: Vec<Used>,
    pub parameters: Option<Value>,
    pub derived_from: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributeType {
    Number,
    Text,
    Bool,
    Date,
}

impl AttributeType {
    pub const ALL: [AttributeType; 4] = [
        AttributeType::Number,
        AttributeType::Text,
        AttributeType::Bool,
        AttributeType::Date,
    ];

    pub fn name(self) -> &'static str {
        match self {
            AttributeType::Number => "number",
            AttributeType::Text => "text",
            AttributeType::Bool => "bool",
            AttributeType::Date => "date",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributeInfo {
    pub kind: AttributeType,
    pub unit: Option<String>,
    pub description: Option<String>,
}

/// An attribute written with a value of another type than the registry holds. The value is
/// written anyway: the registry never drops an edit.
#[derive(Debug, Clone, PartialEq, Error)]
#[error("attribute {key} is registered as {} but was written {written}", registered.name())]
pub struct TypeWarning {
    pub key: String,
    pub registered: AttributeType,
    pub written: Value,
}

/// What writing an imported file did.
#[derive(Debug, Default, PartialEq)]
pub struct Written {
    /// The imported catalogues' uuids, in the file's order.
    pub catalogues: Vec<String>,
    pub warnings: Vec<TypeWarning>,
}

#[derive(Debug, Error)]
pub enum WriteError {
    #[error("{key} is not a value a room document holds")]
    Value {
        key: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("time {0} is outside the years 1677 to 2262 a room holds")]
    Time(Timestamp),
    #[error("confidence {0} is not between 0 and 1")]
    Confidence(f64),
    #[error("a shape needs at least one vertex")]
    EmptyShape,
    #[error(transparent)]
    Rule(#[from] crate::rules::RuleError),
}
