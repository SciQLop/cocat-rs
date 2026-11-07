//! Operators on sets of half-open intervals `[start, stop)` in nanoseconds, named after
//! bedtools (https://bedtools.readthedocs.io/en/latest/content/overview.html). A zero-length
//! item is a point `t`: it lies in an interval when `start <= t < stop`. Touching intervals
//! do not overlap but are at distance zero. Every operator returns its items sorted.

use std::iter;

use super::RuleError;

/// Tiling beyond this many intervals is refused rather than filling memory.
pub const MAX_TILES: usize = 1_000_000;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    /// An existing event, by uuid.
    Event(String),
    /// A derived, read-only interval, with the sorted uuids of the events it came from.
    Derived(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Item {
    pub start: i64,
    pub stop: i64,
    pub source: Source,
    /// The distance, in nanoseconds, to the nearest item `closest` measured against.
    pub gap: Option<i64>,
}

impl Item {
    pub fn event(uuid: impl Into<String>, start: i64, stop: i64) -> Self {
        Self {
            start,
            stop,
            source: Source::Event(uuid.into()),
            gap: None,
        }
    }

    pub fn derived(start: i64, stop: i64, mut sources: Vec<String>) -> Self {
        sources.sort_unstable();
        sources.dedup();
        Self {
            start,
            stop,
            source: Source::Derived(sources),
            gap: None,
        }
    }

    /// The uuids of the events this item stands for.
    pub fn sources(&self) -> &[String] {
        match &self.source {
            Source::Event(uuid) => std::slice::from_ref(uuid),
            Source::Derived(sources) => sources,
        }
    }

    fn is_point(&self) -> bool {
        self.start == self.stop
    }
}

/// A length in seconds as nanoseconds; `as` saturates at the ends of `i64`.
pub fn seconds(value: f64) -> i64 {
    (value * 1e9).round() as i64
}

/// Every item of every set, each event once.
pub fn union(sets: &[Vec<Item>]) -> Vec<Item> {
    let mut items = sorted(sets.concat());
    items.dedup_by(|later, kept| {
        let same =
            (later.start, later.stop, &later.source) == (kept.start, kept.stop, &kept.source);
        if same {
            kept.gap = kept.gap.or(later.gap);
        }
        same
    });
    items
}

/// Joins items that overlap, touch, or lie at most `gap` apart.
pub fn merge(items: &[Item], gap: i64) -> Vec<Item> {
    let mut merged: Vec<(i64, i64, Vec<String>)> = Vec::new();
    for item in sorted(items.to_vec()) {
        match merged.last_mut() {
            Some((_, stop, sources)) if item.start <= stop.saturating_add(gap) => {
                *stop = (*stop).max(item.stop);
                sources.extend_from_slice(item.sources());
            }
            _ => merged.push((item.start, item.stop, item.sources().to_vec())),
        }
    }
    merged
        .into_iter()
        .map(|(start, stop, sources)| Item::derived(start, stop, sources))
        .collect()
}

/// The overlap of every pair of an item of `a` and an item of `b`.
pub fn intersect(a: &[Item], b: &[Item]) -> Vec<Item> {
    let b = sorted(b.to_vec());
    let longest = b
        .iter()
        .map(|item| item.stop.saturating_sub(item.start))
        .max()
        .unwrap_or(0);
    let mut overlaps = Vec::new();
    for x in a {
        // simplify: scans every b starting up to `longest` before x, so one very long
        // interval in b makes this O(|a|·|b|); an interval tree fixes that if it shows.
        let first = b.partition_point(|y| y.start < x.start.saturating_sub(longest));
        let last = b.partition_point(|y| y.start <= x.stop);
        for y in b[first..last].iter().filter(|y| overlap(x, y)) {
            let sources = [x.sources(), y.sources()].concat();
            let (start, stop) = (x.start.max(y.start), x.stop.min(y.stop));
            overlaps.push(Item::derived(start, stop, sources));
        }
    }
    sorted(overlaps)
}

/// What of `a` lies outside `b`. An item `b` leaves whole stays as it is; the pieces of a
/// cut item are derived from it.
pub fn subtract(a: &[Item], b: &[Item]) -> Vec<Item> {
    let (points, intervals): (Vec<Item>, Vec<Item>) = b.iter().cloned().partition(Item::is_point);
    let holes = spans(&merge(&intervals, 0));
    let points: Vec<i64> = points.iter().map(|point| point.start).collect();
    let left = a.iter().flat_map(|item| cut(item, &holes, &points));
    sorted(left.collect())
}

pub fn shift(items: &[Item], by: i64) -> Vec<Item> {
    let moved = items.iter().map(|item| {
        let (start, stop) = (item.start.saturating_add(by), item.stop.saturating_add(by));
        Item::derived(start, stop, item.sources().to_vec())
    });
    sorted(moved.collect())
}

/// Cuts each item into consecutive windows of `width`, the last one cut at the item's stop.
/// A point stays a point.
pub fn tile(items: &[Item], width: i64) -> Result<Vec<Item>, RuleError> {
    let width = width.max(1);
    let count: i128 = items.iter().map(|item| tile_count(item, width)).sum();
    if count > MAX_TILES as i128 {
        return Err(RuleError::TooManyTiles);
    }
    Ok(sorted(
        items.iter().flat_map(|item| tiles(item, width)).collect(),
    ))
}

/// The items of `a` at most `within` from an item of `b`, as they are.
pub fn window(a: &[Item], b: &[Item], within: i64) -> Vec<Item> {
    let near = spans(&merge(b, 0));
    let kept = a
        .iter()
        .filter(|item| gap(&near, item).is_some_and(|gap| gap <= within))
        .cloned();
    sorted(kept.collect())
}

/// The items of `a`, each with its gap to the nearest item of `b` (none when `b` is empty).
pub fn closest(a: &[Item], b: &[Item]) -> Vec<Item> {
    let near = spans(&merge(b, 0));
    let measured = a.iter().map(|item| Item {
        gap: gap(&near, item),
        ..item.clone()
    });
    sorted(measured.collect())
}

fn sorted(mut items: Vec<Item>) -> Vec<Item> {
    items.sort();
    items
}

fn spans(items: &[Item]) -> Vec<(i64, i64)> {
    items.iter().map(|item| (item.start, item.stop)).collect()
}

fn overlap(x: &Item, y: &Item) -> bool {
    match (x.is_point(), y.is_point()) {
        (true, true) => x.start == y.start,
        (true, false) => contains((y.start, y.stop), x.start),
        (false, true) => contains((x.start, x.stop), y.start),
        (false, false) => x.start.max(y.start) < x.stop.min(y.stop),
    }
}

fn contains((start, stop): (i64, i64), t: i64) -> bool {
    start <= t && t < stop
}

/// `item` without the disjoint, sorted `holes` and, for a point, the `points`.
fn cut(item: &Item, holes: &[(i64, i64)], points: &[i64]) -> Vec<Item> {
    let first = holes.partition_point(|(_, stop)| *stop <= item.start);
    if item.is_point() {
        let removed = points.contains(&item.start)
            || holes
                .get(first)
                .is_some_and(|hole| contains(*hole, item.start));
        return if removed { vec![] } else { vec![item.clone()] };
    }
    let inside: Vec<&(i64, i64)> = holes[first..]
        .iter()
        .take_while(|(start, _)| *start < item.stop)
        .collect();
    if inside.is_empty() {
        return vec![item.clone()];
    }
    let piece = |start, stop| Item::derived(start, stop, item.sources().to_vec());
    let mut pieces = Vec::new();
    let mut cursor = item.start;
    for (start, stop) in inside {
        if *start > cursor {
            pieces.push(piece(cursor, *start));
        }
        cursor = cursor.max(*stop);
    }
    if cursor < item.stop {
        pieces.push(piece(cursor, item.stop));
    }
    pieces
}

/// The distance from `item` to the nearest of the disjoint, sorted `spans`.
fn gap(spans: &[(i64, i64)], item: &Item) -> Option<i64> {
    let next = spans.partition_point(|(_, stop)| *stop < item.start);
    spans[next.saturating_sub(1)..]
        .iter()
        .take(2)
        .map(|(start, stop)| {
            item.start
                .max(*start)
                .saturating_sub(item.stop.min(*stop))
                .max(0)
        })
        .min()
}

fn tile_count(item: &Item, width: i64) -> i128 {
    let length = i128::from(item.stop) - i128::from(item.start);
    (length + i128::from(width) - 1)
        .div_euclid(i128::from(width))
        .max(1)
}

fn tiles(item: &Item, width: i64) -> impl Iterator<Item = Item> + '_ {
    let starts = iter::successors(Some(item.start), move |start| {
        start.checked_add(width).filter(|next| *next < item.stop)
    });
    starts.map(move |start| {
        let stop = start.saturating_add(width).min(item.stop);
        Item::derived(start, stop, item.sources().to_vec())
    })
}
