//! A shape as plain JSON, the form VOTable and tscat JSON files carry it in:
//! `[["2025-01-31T10:00:00Z", 12000.0], ...]`, one `[time, value]` pair per vertex. The time
//! is RFC 3339 in UTC, down to the nanosecond a project stores; the value is on the
//! catalogue's shape axis. No pair at all is no shape.

use time::error::Format;

use crate::model::Vertex;
use crate::time::Timestamp;

pub type Pair = (String, f64);

pub fn pairs(shape: &[Vertex]) -> Result<Vec<Pair>, Format> {
    shape
        .iter()
        .map(|vertex| Ok((vertex.time.rfc3339()?, vertex.value)))
        .collect()
}

/// The shape the pairs draw, or the first time that does not read.
pub fn vertices(pairs: &[Pair]) -> Result<Option<Vec<Vertex>>, String> {
    if pairs.is_empty() {
        return Ok(None);
    }
    pairs
        .iter()
        .map(|(time, value)| {
            Timestamp::parse(time)
                .map(|time| Vertex {
                    time,
                    value: *value,
                })
                .ok_or_else(|| time.clone())
        })
        .collect::<Result<_, _>>()
        .map(Some)
}
