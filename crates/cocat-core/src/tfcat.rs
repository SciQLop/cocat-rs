//! TFCat (doi:10.25935/6068-8528): radio features as time–frequency shapes, a GeoJSON
//! profile whose two coordinates are time and frequency. One file is one catalogue.
//!
//! A polygon becomes the event's shape. Any other geometry is kept as written, as JSON text
//! in the event attribute `tfcat_geometry`, and gives the event its time span. The file's
//! coordinate system and field descriptions stay on the catalogue (`tfcat_crs`,
//! `tfcat_fields`), so an export writes them back. The CRS's spectral axis becomes the
//! catalogue's shape axis, and values stay in its unit; an export writes the shape axis back
//! into the CRS.

mod axis;

use std::collections::BTreeMap;
use std::path::Path;

use geojson::{Feature, Geometry, GeometryValue, Position, feature::Id};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use yrs::{ReadTxn, TransactionMut};

use crate::model::{
    self, AttributeInfo, CatalogueSummary, EventRecord, NewCatalogue, ShapeAxis, Vertex,
    WriteError, Written,
};
use crate::time::Timestamp;
use axis::TimeAxis;

/// The event attribute holding a geometry other than a polygon, as JSON text.
pub const GEOMETRY: &str = "tfcat_geometry";
/// The catalogue attribute holding the file's coordinate system, as JSON text.
pub const CRS: &str = "tfcat_crs";
/// The catalogue attribute holding the file's field descriptions, as JSON text.
pub const FIELDS: &str = "tfcat_fields";

#[derive(Debug, thiserror::Error)]
pub enum TfcatError {
    #[error("Import TFCat: the file is not TFCat: {0}")]
    Format(#[from] serde_json::Error),
    #[error(
        "Import TFCat: the file does not say how to read its times and frequencies (it has no \"crs\")"
    )]
    NoCrs,
    #[error("TFCat: cannot read the time axis: {0}")]
    Crs(String),
    #[error("TFCat: the time {0} is outside the years 1677 to 2262 a project holds")]
    OutOfRange(f64),
    #[error("TFCat: a position needs a time and a frequency, not {0:?}")]
    Position(Vec<f64>),
    #[error("TFCat: the time {0} cannot be written")]
    Time(Timestamp),
    #[error("These events have no {0}: give {article} {0} range", article = article(.0))]
    NoValues(String),
    #[error("Export TFCat: cannot write the file: {0}")]
    Encoding(serde_json::Error),
}

#[derive(Serialize, Deserialize)]
enum CollectionType {
    FeatureCollection,
}

#[derive(Serialize, Deserialize)]
struct Collection {
    #[serde(rename = "type")]
    kind: CollectionType,
    features: Vec<Feature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    properties: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fields: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    crs: Option<Value>,
}

/// A TFCat file read and converted, ready to write into a project as one catalogue.
#[derive(Debug, Clone, PartialEq)]
pub struct TfcatImport {
    /// The collection's `title`, which names the catalogue.
    pub title: Option<String>,
    /// The collection's other properties, its coordinate system and its fields.
    pub attributes: Map<String, Value>,
    /// The CRS's spectral axis.
    pub shape_axis: Option<ShapeAxis>,
    pub events: Vec<EventRecord>,
    /// What could not come across as it was, in words.
    pub warnings: Vec<String>,
}

impl TfcatImport {
    /// The name asked for, else the file's title, else the file's name without extension.
    pub fn name(&self, asked: Option<&str>, file_name: &str) -> String {
        let stem = || {
            Path::new(file_name)
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        asked
            .or(self.title.as_deref())
            .map(str::to_owned)
            .unwrap_or_else(stem)
    }
}

pub fn parse(text: &str) -> Result<TfcatImport, TfcatError> {
    let collection: Collection = serde_json::from_str(text)?;
    let crs = collection.crs.ok_or(TfcatError::NoCrs)?;
    let axis = TimeAxis::of(&crs)?;
    let mut events = Vec::with_capacity(collection.features.len());
    let (mut holes, mut unlocated) = (false, 0);
    for feature in collection.features {
        match event_of(&axis, feature)? {
            Some((event, holed)) => {
                holes |= holed;
                events.push(event);
            }
            None => unlocated += 1,
        }
    }
    let mut attributes = collection.properties.unwrap_or_default();
    let title = match attributes.remove("title") {
        Some(Value::String(title)) => Some(title),
        Some(other) => {
            attributes.insert("title".to_owned(), other);
            None
        }
        None => None,
    };
    let shape_axis = spectral_axis(&crs);
    attributes.insert(CRS.to_owned(), Value::from(crs.to_string()));
    if let Some(fields) = collection.fields.filter(|fields| !fields.is_null()) {
        attributes.insert(FIELDS.to_owned(), Value::from(fields.to_string()));
    }
    Ok(TfcatImport {
        title,
        attributes,
        shape_axis,
        events,
        warnings: warnings(holes, unlocated),
    })
}

fn warnings(holes: bool, unlocated: usize) -> Vec<String> {
    let mut warnings = Vec::new();
    if holes {
        warnings.push("Some shapes had holes: only their outline was kept.".to_owned());
    }
    if unlocated > 0 {
        warnings.push(format!(
            "{unlocated} feature(s) have no position in time and frequency and were left out."
        ));
    }
    warnings
}

/// What a feature's geometry becomes: a shape, or the geometry kept as it is.
enum Placed {
    Shape { ring: Vec<Position>, holes: bool },
    Kept(Geometry),
}

/// The event a feature becomes, and whether its polygon lost holes; `None` for a feature
/// placed nowhere.
fn event_of(axis: &TimeAxis, feature: Feature) -> Result<Option<(EventRecord, bool)>, TfcatError> {
    let Some(geometry) = feature.geometry else {
        return Ok(None);
    };
    let mut attributes = feature.properties.unwrap_or_default();
    let (shape, times, holes) = match placed(geometry) {
        Placed::Shape { ring, holes } => {
            let shape = vertices(axis, &ring)?;
            let times: Vec<Timestamp> = shape.iter().map(|vertex| vertex.time).collect();
            (Some(shape), times, holes)
        }
        Placed::Kept(geometry) => {
            let times = times(axis, &geometry)?;
            attributes.insert(GEOMETRY.to_owned(), Value::from(geometry_text(&geometry)?));
            (None, times, false)
        }
    };
    let (Some(start), Some(stop)) = (times.iter().min(), times.iter().max()) else {
        return Ok(None);
    };
    let mut event = EventRecord::new(uuid::Uuid::new_v4().to_string(), *start, *stop);
    event.shape = shape;
    event.source_id = feature.id.map(|id| match id {
        Id::String(text) => text,
        Id::Number(number) => number.to_string(),
    });
    event.attributes = attributes;
    Ok(Some((event, holes)))
}

fn placed(geometry: Geometry) -> Placed {
    let rings = match &geometry.value {
        GeometryValue::Polygon { coordinates } => Some(coordinates),
        GeometryValue::MultiPolygon { coordinates } if coordinates.len() == 1 => {
            coordinates.first()
        }
        _ => None,
    };
    match rings.and_then(|rings| Some((rings.first()?, rings.len() > 1))) {
        Some((outer, holes)) if !outer.is_empty() => Placed::Shape {
            ring: open_ring(outer),
            holes,
        },
        _ => Placed::Kept(geometry),
    }
}

/// A linear ring without its closing position.
fn open_ring(ring: &[Position]) -> Vec<Position> {
    match ring {
        [first, .., last] if first == last => ring[..ring.len() - 1].to_vec(),
        _ => ring.to_vec(),
    }
}

fn vertices(axis: &TimeAxis, ring: &[Position]) -> Result<Vec<Vertex>, TfcatError> {
    ring.iter()
        .map(|position| {
            let (time, value) = pair(position)?;
            Ok(Vertex {
                time: axis.instant(time)?,
                value,
            })
        })
        .collect()
}

fn times(axis: &TimeAxis, geometry: &Geometry) -> Result<Vec<Timestamp>, TfcatError> {
    positions(geometry)
        .into_iter()
        .map(|position| axis.instant(pair(position)?.0))
        .collect()
}

fn positions(geometry: &Geometry) -> Vec<&Position> {
    match &geometry.value {
        GeometryValue::Point { coordinates } => vec![coordinates],
        GeometryValue::MultiPoint { coordinates } | GeometryValue::LineString { coordinates } => {
            coordinates.iter().collect()
        }
        GeometryValue::MultiLineString { coordinates } | GeometryValue::Polygon { coordinates } => {
            coordinates.iter().flatten().collect()
        }
        GeometryValue::MultiPolygon { coordinates } => {
            coordinates.iter().flatten().flatten().collect()
        }
        GeometryValue::GeometryCollection { geometries } => {
            geometries.iter().flat_map(positions).collect()
        }
    }
}

fn pair(position: &Position) -> Result<(f64, f64), TfcatError> {
    match position.as_slice() {
        [time, value] => Ok((*time, *value)),
        other => Err(TfcatError::Position(other.to_vec())),
    }
}

fn geometry_text(geometry: &Geometry) -> Result<String, TfcatError> {
    serde_json::to_string(geometry).map_err(TfcatError::Encoding)
}

/// Writes the import as a new catalogue named `name` holding its events, in the caller's
/// transaction. `now` is the catalogue's creation time.
pub fn write(
    txn: &mut TransactionMut<'_>,
    import: &TfcatImport,
    name: &str,
    now: Timestamp,
) -> Result<Written, WriteError> {
    let mut written = Written::default();
    for event in &import.events {
        written.warnings.extend(model::create_event(txn, event)?);
    }
    let catalogue = NewCatalogue {
        uuid: uuid::Uuid::new_v4().to_string(),
        name: name.to_owned(),
        created: Some(now),
        attributes: import.attributes.clone(),
        ..NewCatalogue::default()
    };
    model::create_catalogue(txn, &catalogue)?;
    model::set_catalogue_shape_axis(txn, &catalogue.uuid, import.shape_axis.as_ref())?;
    let uuids: Vec<String> = import
        .events
        .iter()
        .map(|event| event.uuid.clone())
        .collect();
    model::add_catalogue_events(txn, &catalogue.uuid, &uuids);
    written.catalogues.push(catalogue.uuid);
    Ok(written)
}

/// Whether exporting these events needs a value range from the caller: some have no place
/// on the shape axis, and none of the others gives a band to borrow.
pub fn needs_value_range(events: &[EventRecord]) -> bool {
    events.iter().any(|event| !is_placed(event)) && band(events).is_none()
}

/// What the catalogue's shapes measure: its own shape axis, else the spectral axis of the
/// TFCat file it came from, else frequency in Hz. A value range is on this axis.
pub fn shape_axis(catalogue: &CatalogueSummary) -> ShapeAxis {
    catalogue.shape_axis.clone().unwrap_or_else(|| {
        let crs = stored(&catalogue.attributes, CRS).unwrap_or_else(default_crs);
        spectral_axis(&crs).unwrap_or_else(default_axis)
    })
}

fn spectral_axis(crs: &Value) -> Option<ShapeAxis> {
    let coords = &crs["properties"]["spectral_coords"];
    Some(ShapeAxis {
        quantity: coords["type"].as_str()?.to_owned(),
        unit: coords["unit"].as_str()?.to_owned(),
    })
}

fn default_axis() -> ShapeAxis {
    ShapeAxis {
        quantity: "frequency".to_owned(),
        unit: "Hz".to_owned(),
    }
}

/// The CRS a file gets: the one it came with, else the default, with the catalogue's shape
/// axis as its spectral axis.
fn crs_of(catalogue: &CatalogueSummary) -> Value {
    let mut crs = stored(&catalogue.attributes, CRS).unwrap_or_else(default_crs);
    if let Some(axis) = &catalogue.shape_axis {
        let coords = &mut crs["properties"]["spectral_coords"];
        coords["type"] = Value::from(axis.quantity.clone());
        coords["unit"] = Value::from(axis.unit.clone());
    }
    crs
}

fn is_placed(event: &EventRecord) -> bool {
    event.shape.is_some() || kept_geometry(event).is_some()
}

/// The lowest and highest value of the events placed on the shape axis.
fn band(events: &[EventRecord]) -> Option<(f64, f64)> {
    let values: Vec<f64> = events.iter().flat_map(values).collect();
    let low = values.iter().copied().reduce(f64::min)?;
    let high = values.iter().copied().reduce(f64::max)?;
    Some((low, high))
}

fn values(event: &EventRecord) -> Vec<f64> {
    match (&event.shape, kept_geometry(event)) {
        (Some(shape), _) => shape.iter().map(|vertex| vertex.value).collect(),
        (None, Some(geometry)) => positions(&geometry)
            .into_iter()
            .filter_map(|position| pair(position).ok().map(|(_, value)| value))
            .collect(),
        (None, None) => Vec::new(),
    }
}

fn kept_geometry(event: &EventRecord) -> Option<Geometry> {
    let text = event.attributes.get(GEOMETRY)?.as_str()?;
    serde_json::from_str(text).ok()
}

/// "a" or "an", for the error that names the axis.
fn article(word: &str) -> &'static str {
    match word.starts_with(['a', 'e', 'i', 'o', 'u']) {
        true => "an",
        false => "a",
    }
}

fn stored(attributes: &Map<String, Value>, key: &str) -> Option<Value> {
    serde_json::from_str(attributes.get(key)?.as_str()?).ok()
}

/// The reference Python package's default: Unix time and frequency in Hz, seen from the
/// Earth's centre.
fn default_crs() -> Value {
    json!({
        "type": "local",
        "properties": {
            "name": "Time-Frequency",
            "time_coords_id": "unix",
            "spectral_coords": {"type": "frequency", "unit": "Hz"},
            "ref_position_id": "GEOCENTER",
        },
    })
}

/// The catalogue as one TFCat file, its times written on its own time axis. An event
/// without a place on the shape axis becomes a rectangle over `value_range`, else over the
/// band of the catalogue's other events.
pub fn export<T: ReadTxn>(
    txn: &T,
    catalogue: &CatalogueSummary,
    events: &[EventRecord],
    value_range: Option<(f64, f64)>,
) -> Result<String, TfcatError> {
    let crs = crs_of(catalogue);
    let axis = TimeAxis::of(&crs)?;
    let band = value_range.or_else(|| band(events));
    let quantity = shape_axis(catalogue).quantity;
    let features = in_time_order(events)
        .into_iter()
        .map(|event| feature(&axis, event, band, &quantity))
        .collect::<Result<_, _>>()?;
    let collection = Collection {
        kind: CollectionType::FeatureCollection,
        features,
        properties: Some(collection_properties(catalogue)),
        fields: Some(Value::Object(fields(txn, catalogue, events))),
        crs: Some(crs),
    };
    serde_json::to_string(&collection).map_err(TfcatError::Encoding)
}

fn in_time_order(events: &[EventRecord]) -> Vec<&EventRecord> {
    let mut ordered: Vec<&EventRecord> = events.iter().collect();
    ordered.sort_by(|left, right| (left.start, &left.uuid).cmp(&(right.start, &right.uuid)));
    ordered
}

fn feature(
    axis: &TimeAxis,
    event: &EventRecord,
    band: Option<(f64, f64)>,
    quantity: &str,
) -> Result<Feature, TfcatError> {
    let geometry = match (kept_geometry(event), &event.shape) {
        (Some(geometry), _) => geometry,
        (None, Some(shape)) => {
            let corners: Vec<(Timestamp, f64)> = shape
                .iter()
                .map(|vertex| (vertex.time, vertex.value))
                .collect();
            polygon(axis, &corners)?
        }
        (None, None) => {
            let (low, high) = band.ok_or_else(|| TfcatError::NoValues(quantity.to_owned()))?;
            let (start, stop) = (event.start, event.stop);
            polygon(
                axis,
                &[(start, low), (stop, low), (stop, high), (start, high)],
            )?
        }
    };
    let mut properties = event.attributes.clone();
    properties.remove(GEOMETRY);
    Ok(Feature {
        bbox: None,
        geometry: Some(geometry),
        id: Some(feature_id(event)),
        properties: Some(properties),
        foreign_members: None,
    })
}

/// A closed polygon through the corners.
fn polygon(axis: &TimeAxis, corners: &[(Timestamp, f64)]) -> Result<Geometry, TfcatError> {
    let mut ring = corners
        .iter()
        .map(|(time, value)| Ok(Position::from([axis.value(*time)?, *value])))
        .collect::<Result<Vec<_>, TfcatError>>()?;
    if let Some(first) = ring.first().cloned() {
        ring.push(first);
    }
    Ok(Geometry::new(GeometryValue::Polygon {
        coordinates: vec![ring],
    }))
}

/// The source id, as the number it was when it reads as one; else the event's uuid.
fn feature_id(event: &EventRecord) -> Id {
    let Some(source) = &event.source_id else {
        return Id::String(event.uuid.clone());
    };
    match source.parse::<i64>() {
        Ok(number) if number.to_string() == *source => Id::Number(number.into()),
        _ => Id::String(source.clone()),
    }
}

fn collection_properties(catalogue: &CatalogueSummary) -> Map<String, Value> {
    let mut properties = catalogue.attributes.clone();
    properties.remove(CRS);
    properties.remove(FIELDS);
    properties.insert("title".to_owned(), Value::from(catalogue.name.clone()));
    properties
}

/// The file's own field descriptions, and one for each other attribute the events carry.
fn fields<T: ReadTxn>(
    txn: &T,
    catalogue: &CatalogueSummary,
    events: &[EventRecord],
) -> Map<String, Value> {
    let mut fields = match stored(&catalogue.attributes, FIELDS) {
        Some(Value::Object(fields)) => fields,
        _ => Map::new(),
    };
    let registry = model::attribute_types(txn);
    for (key, value) in events.iter().flat_map(|event| &event.attributes) {
        if key != GEOMETRY && !fields.contains_key(key) {
            fields.insert(key.clone(), field(key, value, &registry));
        }
    }
    fields
}

/// A field description (spec §3.3.1). cocat has no UCD for an attribute, so `ucd` is empty.
fn field(key: &str, value: &Value, registry: &BTreeMap<String, AttributeInfo>) -> Value {
    let info = registry.get(key);
    let mut field = json!({
        "info": info.and_then(|info| info.description.clone()).unwrap_or_else(|| key.to_owned()),
        "datatype": datatype(value),
        "ucd": "",
    });
    if let Some(unit) = info.and_then(|info| info.unit.clone()) {
        field["unit"] = Value::from(unit);
    }
    field
}

fn datatype(value: &Value) -> &'static str {
    match value {
        Value::Bool(_) => "bool",
        Value::Number(number) if number.is_f64() => "float",
        Value::Number(_) => "int",
        _ => "str",
    }
}

#[cfg(test)]
mod tests;
