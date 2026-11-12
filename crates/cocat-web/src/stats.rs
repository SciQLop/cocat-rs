//! What the events on screen add up to: how much time they cover, how long they last, how far
//! apart they are, and how their tags, statuses, attributes and authors spread.
//!
//! The summary is cheap enough to follow every keystroke of the filter; the other sections
//! are their own functions so that the page computes them only while they are open.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap};

use cocat_core::model::Status;
use serde_json::Value;

use crate::format::value_text;
use crate::rows::Row;

const SECOND: i64 = 1_000_000_000;
/// The log histograms cover 1 s to 10^6 s (11.6 days) in half-decades; what falls outside
/// counts in the first or last bin.
pub const LOG_BINS: usize = 12;
const LOG_DECADES: f64 = 6.0;
/// Where the log histograms mark familiar lengths.
pub const LOG_TICKS: [(&str, i64); 5] = [
    ("1 s", SECOND),
    ("1 min", 60 * SECOND),
    ("1 h", 3_600 * SECOND),
    ("1 d", 86_400 * SECOND),
    ("1 w", 7 * 86_400 * SECOND),
];
pub const CONFIDENCE_BINS: usize = 10;
pub const TOP_TAGS: usize = 5;
const TOP_PAIRS: usize = 5;
const TOP_VALUES: usize = 3;

/// The always-shown summary of the events on screen.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CatalogueStats {
    pub count: usize,
    /// The first start and the last stop.
    pub span: Option<(i64, i64)>,
    /// The time at least one event covers: overlaps count once.
    pub covered: i64,
    pub overlapping_pairs: u64,
    pub duration: Option<Spread>,
    pub durations: Vec<u32>,
    pub tags: Counted,
}

impl CatalogueStats {
    /// The covered time as a percentage of the span; none for a span of no length.
    pub fn coverage_percent(&self) -> Option<f64> {
        let (first, last) = self.span?;
        (last > first).then(|| self.covered as f64 / (last - first) as f64 * 100.0)
    }
}

/// The least, the median and the greatest of some lengths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spread {
    pub min: i64,
    pub median: i64,
    pub max: i64,
}

/// How many events have each value, most frequent first, and how many have none.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Counted {
    pub shares: Vec<Share>,
    pub missing: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    pub name: String,
    pub count: usize,
}

/// The waiting times from one event's stop to the next one's start.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Gaps {
    pub spread: Option<Spread>,
    pub bins: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Statuses {
    /// Only the statuses some event has, in the order a status goes through.
    pub counts: Vec<(Status, usize)>,
    pub confidence_mean: Option<f64>,
    pub confidence_bins: Vec<u32>,
    pub without_confidence: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttributeStats {
    pub key: String,
    pub present: usize,
    pub values: Values,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Values {
    Numbers { min: f64, median: f64, max: f64 },
    Text { distinct: usize, top: Vec<Share> },
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct TagStats {
    pub tags: Counted,
    /// The tags most often found together on one event.
    pub pairs: Vec<(String, String, usize)>,
}

pub fn summary(rows: &[&Row]) -> CatalogueStats {
    let ranges = sorted_ranges(rows);
    let durations: Vec<i64> = ranges.iter().map(|(start, stop)| stop - start).collect();
    let mut tags = count_by(rows, |row| &row.tags);
    tags.shares.truncate(TOP_TAGS);
    CatalogueStats {
        count: rows.len(),
        span: span(&ranges),
        covered: covered(&ranges),
        overlapping_pairs: overlapping_pairs(&ranges),
        durations: log_bins(&durations),
        duration: spread(durations),
        tags,
    }
}

pub fn gaps(rows: &[&Row]) -> Gaps {
    let waits = waits(&sorted_ranges(rows));
    Gaps {
        bins: log_bins(&waits),
        spread: spread(waits),
    }
}

pub fn statuses(rows: &[&Row]) -> Statuses {
    let counts = Status::ALL
        .iter()
        .map(|&status| {
            (
                status,
                rows.iter().filter(|row| row.status == status).count(),
            )
        })
        .filter(|&(_, count)| count > 0)
        .collect();
    let confidences: Vec<f64> = rows.iter().filter_map(|row| row.confidence).collect();
    let mut confidence_bins = vec![0; CONFIDENCE_BINS];
    for confidence in &confidences {
        let bin = (confidence.max(0.0) * CONFIDENCE_BINS as f64).floor() as usize;
        confidence_bins[bin.min(CONFIDENCE_BINS - 1)] += 1;
    }
    Statuses {
        counts,
        confidence_mean: (!confidences.is_empty())
            .then(|| confidences.iter().sum::<f64>() / confidences.len() as f64),
        confidence_bins,
        without_confidence: rows.len() - confidences.len(),
    }
}

/// One summary per attribute key, by key. A key whose values are all numbers gets their
/// range; any other gets its most frequent values as text.
pub fn attributes(rows: &[&Row]) -> Vec<AttributeStats> {
    let mut by_key: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for row in rows {
        for (key, value) in &row.attributes {
            by_key.entry(key).or_default().push(value);
        }
    }
    by_key
        .into_iter()
        .map(|(key, values)| AttributeStats {
            key: key.to_owned(),
            present: values.len(),
            values: summarise(&values),
        })
        .collect()
}

pub fn all_tags(rows: &[&Row]) -> TagStats {
    TagStats {
        tags: count_by(rows, |row| &row.tags),
        pairs: pairs(rows),
    }
}

pub fn authors(rows: &[&Row]) -> Counted {
    count_by(rows, |row| row.author.as_slice())
}

/// Where a length sits on a log histogram's axis, in percent.
pub fn log_position(nanos: i64) -> f64 {
    (decades(nanos) / LOG_DECADES * 100.0).min(100.0)
}

/// "412 events", "1 event", "37 of 412 events".
pub fn heading(shown: usize, total: usize) -> String {
    match (shown, total) {
        (0, 0) => "No events".to_owned(),
        (1, 1) => "1 event".to_owned(),
        _ if shown == total => format!("{shown} events"),
        _ => format!("{shown} of {total} events"),
    }
}

/// "1.2 %", "37 %", "< 0.1 %".
pub fn percent_text(percent: f64) -> String {
    match percent {
        0.0 => "0 %".to_owned(),
        _ if percent < 0.1 => "< 0.1 %".to_owned(),
        _ if percent < 10.0 => format!("{percent:.1} %"),
        _ => format!("{percent:.0} %"),
    }
}

/// A number as short as it reads well: "3", "1.25", "0.333".
pub fn number_text(number: f64) -> String {
    let text = format!("{number:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    match text {
        "-0" => "0".to_owned(),
        _ => text.to_owned(),
    }
}

/// Every event's start and stop, by start then stop.
fn sorted_ranges(rows: &[&Row]) -> Vec<(i64, i64)> {
    let mut ranges: Vec<(i64, i64)> = rows.iter().map(|row| (row.start, row.stop)).collect();
    ranges.sort_unstable();
    ranges
}

fn span(sorted: &[(i64, i64)]) -> Option<(i64, i64)> {
    let first = sorted.first()?.0;
    let last = sorted.iter().map(|&(_, stop)| stop).max()?;
    Some((first, last))
}

/// The length of the union of `sorted`'s intervals.
fn covered(sorted: &[(i64, i64)]) -> i64 {
    let mut total = 0;
    let mut reach = i64::MIN;
    for &(start, stop) in sorted {
        let from = start.max(reach);
        if stop > from {
            total += stop - from;
            reach = stop;
        }
    }
    total
}

/// Pairs that share some time: each start meets the events still open. Sorting by stop
/// after start keeps an instant from counting an event that only starts at it.
fn overlapping_pairs(sorted: &[(i64, i64)]) -> u64 {
    let mut open = BinaryHeap::new();
    let mut pairs = 0;
    for &(start, stop) in sorted {
        while open.peek().is_some_and(|&Reverse(end)| end <= start) {
            open.pop();
        }
        pairs += open.len() as u64;
        open.push(Reverse(stop));
    }
    pairs
}

/// From the latest stop so far to the next start; a start before it overlaps and waits not.
fn waits(sorted: &[(i64, i64)]) -> Vec<i64> {
    let mut waits = Vec::new();
    let mut reach: Option<i64> = None;
    for &(start, stop) in sorted {
        if let Some(reached) = reach.filter(|&reached| start >= reached) {
            waits.push(start - reached);
        }
        reach = Some(reach.map_or(stop, |reached| reached.max(stop)));
    }
    waits
}

fn spread(mut values: Vec<i64>) -> Option<Spread> {
    values.sort_unstable();
    Some(Spread {
        min: *values.first()?,
        median: middle(&values, |low, high| low + (high - low) / 2)?,
        max: *values.last()?,
    })
}

/// The median of sorted values: the middle one, or `halfway` between the middle two.
fn middle<T: Copy>(sorted: &[T], halfway: impl Fn(T, T) -> T) -> Option<T> {
    let half = sorted.len() / 2;
    let upper = *sorted.get(half)?;
    Some(match sorted.len() % 2 {
        0 => halfway(sorted[half - 1], upper),
        _ => upper,
    })
}

fn decades(nanos: i64) -> f64 {
    let seconds = nanos as f64 / SECOND as f64;
    if seconds <= 1.0 { 0.0 } else { seconds.log10() }
}

fn log_bins(lengths: &[i64]) -> Vec<u32> {
    let per_decade = LOG_BINS as f64 / LOG_DECADES;
    let mut bins = vec![0; LOG_BINS];
    for &length in lengths {
        let bin = (decades(length) * per_decade).floor() as usize;
        bins[bin.min(LOG_BINS - 1)] += 1;
    }
    bins
}

fn count_by<'r>(rows: &[&'r Row], values: impl Fn(&'r Row) -> &'r [String]) -> Counted {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    let mut missing = 0;
    for &row in rows {
        let values = values(row);
        if values.is_empty() {
            missing += 1;
        }
        for value in values {
            *counts.entry(value).or_default() += 1;
        }
    }
    Counted {
        shares: ranked(
            counts
                .into_iter()
                .map(|(name, count)| (name.to_owned(), count)),
        ),
        missing,
    }
}

/// Most frequent first; ties by name.
fn ranked(counts: impl IntoIterator<Item = (String, usize)>) -> Vec<Share> {
    let mut shares: Vec<Share> = counts
        .into_iter()
        .map(|(name, count)| Share { name, count })
        .collect();
    shares.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.name.cmp(&right.name))
    });
    shares
}

fn summarise(values: &[&Value]) -> Values {
    let numbers: Option<Vec<f64>> = values.iter().map(|value| value.as_f64()).collect();
    match numbers {
        Some(mut numbers) if !numbers.is_empty() => {
            numbers.sort_by(f64::total_cmp);
            Values::Numbers {
                min: numbers[0],
                median: middle(&numbers, |low, high| (low + high) / 2.0).unwrap_or_default(),
                max: numbers[numbers.len() - 1],
            }
        }
        _ => {
            let mut counts: HashMap<String, usize> = HashMap::new();
            for value in values {
                *counts.entry(value_text(value)).or_default() += 1;
            }
            let distinct = counts.len();
            let mut top = ranked(counts);
            top.truncate(TOP_VALUES);
            Values::Text { distinct, top }
        }
    }
}

// simplify: every tag of an event pairs with every other, k² per event; fine for the few tags
// an event has, worth a cap on k if catalogues with dozens of tags per event show up.
fn pairs(rows: &[&Row]) -> Vec<(String, String, usize)> {
    let mut counts: HashMap<(&str, &str), usize> = HashMap::new();
    for row in rows {
        let mut tags: Vec<&str> = row.tags.iter().map(String::as_str).collect();
        tags.sort_unstable();
        tags.dedup();
        for (index, &first) in tags.iter().enumerate() {
            for &second in &tags[index + 1..] {
                *counts.entry((first, second)).or_default() += 1;
            }
        }
    }
    let mut pairs: Vec<(String, String, usize)> = counts
        .into_iter()
        .map(|((first, second), count)| (first.to_owned(), second.to_owned(), count))
        .collect();
    pairs.sort_by(|left, right| {
        right
            .2
            .cmp(&left.2)
            .then_with(|| left.0.cmp(&right.0))
            .then_with(|| left.1.cmp(&right.1))
    });
    pairs.truncate(TOP_PAIRS);
    pairs
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};

    use super::*;

    const MINUTE: i64 = 60 * SECOND;
    const HOUR: i64 = 3_600 * SECOND;

    fn row(start: i64, stop: i64) -> Row {
        Row {
            uuid: Some(format!("{start}-{stop}")),
            start,
            stop,
            tags: Vec::new(),
            confidence: None,
            status: Status::Confirmed,
            author: None,
            attributes: Map::new(),
            sources: Vec::new(),
        }
    }

    fn tagged(start: i64, tags: &[&str]) -> Row {
        Row {
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            ..row(start, start + SECOND)
        }
    }

    fn with(value: Value) -> Row {
        Row {
            attributes: value.as_object().cloned().unwrap(),
            ..row(0, SECOND)
        }
    }

    fn refs(rows: &[Row]) -> Vec<&Row> {
        rows.iter().collect()
    }

    fn share(name: &str, count: usize) -> Share {
        Share {
            name: name.into(),
            count,
        }
    }

    #[test]
    fn no_events_have_no_statistics() {
        let stats = summary(&[]);
        assert_eq!(stats.count, 0);
        assert_eq!(stats.span, None);
        assert_eq!(stats.covered, 0);
        assert_eq!(stats.duration, None);
        assert_eq!(stats.coverage_percent(), None);
        assert_eq!(stats.durations, vec![0; LOG_BINS]);
        assert_eq!(
            gaps(&[]),
            Gaps {
                spread: None,
                bins: vec![0; LOG_BINS]
            }
        );
        assert!(attributes(&[]).is_empty());
        assert_eq!(statuses(&[]).confidence_mean, None);
    }

    #[test]
    fn one_event_covers_its_whole_span() {
        let rows = [row(10 * HOUR, 12 * HOUR)];
        let stats = summary(&refs(&rows));
        assert_eq!(stats.count, 1);
        assert_eq!(stats.span, Some((10 * HOUR, 12 * HOUR)));
        assert_eq!(stats.covered, 2 * HOUR);
        assert_eq!(stats.coverage_percent(), Some(100.0));
        assert_eq!(stats.overlapping_pairs, 0);
        let two_hours = 2 * HOUR;
        assert_eq!(
            stats.duration,
            Some(Spread {
                min: two_hours,
                median: two_hours,
                max: two_hours
            })
        );
        assert_eq!(gaps(&refs(&rows)).spread, None);
    }

    #[test]
    fn overlapping_events_count_their_shared_time_once() {
        let rows = [
            row(0, 2 * HOUR),
            row(HOUR, 3 * HOUR),
            row(5 * HOUR, 6 * HOUR),
        ];
        let stats = summary(&refs(&rows));
        assert_eq!(stats.covered, 4 * HOUR);
        assert_eq!(stats.span, Some((0, 6 * HOUR)));
        let percent = stats.coverage_percent().unwrap();
        assert!((percent - 400.0 / 6.0).abs() < 1e-9, "{percent}");
        assert_eq!(stats.overlapping_pairs, 1);
    }

    #[test]
    fn a_nested_event_adds_no_coverage_and_overlaps_its_host() {
        let rows = [
            row(0, 10 * HOUR),
            row(2 * HOUR, 3 * HOUR),
            row(4 * HOUR, 5 * HOUR),
        ];
        let stats = summary(&refs(&rows));
        assert_eq!(stats.covered, 10 * HOUR);
        assert_eq!(stats.overlapping_pairs, 2);
    }

    #[test]
    fn the_span_ends_at_the_latest_stop_not_the_last_start() {
        let rows = [row(0, 10 * HOUR), row(HOUR, 2 * HOUR)];
        assert_eq!(summary(&refs(&rows)).span, Some((0, 10 * HOUR)));
    }

    #[test]
    fn zero_length_events_cover_nothing_and_overlap_only_what_they_fall_inside() {
        let rows = [
            row(0, 2 * HOUR),
            row(HOUR, HOUR),
            row(0, 0),
            row(2 * HOUR, 2 * HOUR),
            row(5 * HOUR, 5 * HOUR),
            row(5 * HOUR, 5 * HOUR),
        ];
        let stats = summary(&refs(&rows));
        assert_eq!(stats.covered, 2 * HOUR);
        assert_eq!(stats.overlapping_pairs, 1);
        assert_eq!(stats.duration.unwrap().min, 0);
        assert_eq!(stats.durations[0], 5);
    }

    #[test]
    fn touching_events_do_not_overlap() {
        let rows = [row(0, HOUR), row(HOUR, 2 * HOUR)];
        let stats = summary(&refs(&rows));
        assert_eq!(stats.overlapping_pairs, 0);
        assert_eq!(stats.covered, 2 * HOUR);
    }

    #[test]
    fn the_median_of_an_odd_count_is_the_middle_one() {
        let rows = [row(0, MINUTE), row(0, HOUR), row(0, 5 * SECOND)];
        assert_eq!(
            summary(&refs(&rows)).duration,
            Some(Spread {
                min: 5 * SECOND,
                median: MINUTE,
                max: HOUR
            })
        );
    }

    #[test]
    fn the_median_of_an_even_count_is_halfway_between_the_middle_two() {
        let rows = [
            row(0, MINUTE),
            row(0, HOUR),
            row(0, 3 * MINUTE),
            row(0, 5 * SECOND),
        ];
        assert_eq!(summary(&refs(&rows)).duration.unwrap().median, 2 * MINUTE);
    }

    #[test]
    fn durations_fall_in_half_decade_bins() {
        let rows = [
            row(0, SECOND),
            row(0, 2 * SECOND),
            row(0, 10 * SECOND),
            row(0, MINUTE),
            row(0, HOUR),
            row(0, 86_400 * SECOND),
            row(0, 100 * 86_400 * SECOND),
        ];
        let mut expected = vec![0; LOG_BINS];
        expected[0] = 2;
        expected[2] = 1;
        expected[3] = 1;
        expected[7] = 1;
        expected[9] = 1;
        expected[11] = 1;
        assert_eq!(summary(&refs(&rows)).durations, expected);
    }

    #[test]
    fn familiar_lengths_sit_at_their_log_position() {
        assert_eq!(log_position(SECOND), 0.0);
        assert_eq!(log_position(1_000_000 * SECOND), 100.0);
        assert!((log_position(1_000 * SECOND) - 50.0).abs() < 1e-9);
        assert_eq!(log_position(0), 0.0);
        assert_eq!(log_position(i64::MAX), 100.0);
    }

    #[test]
    fn tags_count_events_most_frequent_first_and_untagged_ones_apart() {
        let rows = [
            tagged(0, &["shock", "icme"]),
            tagged(1, &["shock"]),
            tagged(2, &[]),
            tagged(3, &["icme", "shock"]),
            tagged(4, &["flux rope"]),
        ];
        let tags = summary(&refs(&rows)).tags;
        assert_eq!(
            tags.shares,
            [share("shock", 3), share("icme", 2), share("flux rope", 1)]
        );
        assert_eq!(tags.missing, 1);
    }

    #[test]
    fn the_summary_keeps_the_top_five_tags() {
        let rows: Vec<Row> = (0..7)
            .map(|index| {
                tagged(
                    index,
                    &[["a", "b", "c", "d", "e", "f", "g"][index as usize]],
                )
            })
            .collect();
        assert_eq!(summary(&refs(&rows)).tags.shares.len(), TOP_TAGS);
        assert_eq!(all_tags(&refs(&rows)).tags.shares.len(), 7);
    }

    #[test]
    fn tags_found_together_count_as_pairs() {
        let rows = [
            tagged(0, &["shock", "icme", "sheath"]),
            tagged(1, &["icme", "shock"]),
            tagged(2, &["shock"]),
        ];
        let pairs = all_tags(&refs(&rows)).pairs;
        assert_eq!(pairs[0], ("icme".into(), "shock".into(), 2));
        assert_eq!(pairs.len(), 3);
    }

    #[test]
    fn gaps_run_from_a_stop_to_the_next_start_and_skip_overlaps() {
        let rows = [
            row(10 * HOUR, 11 * HOUR),
            row(0, 4 * HOUR),
            row(HOUR, 2 * HOUR),
            row(3 * HOUR, 5 * HOUR),
            row(7 * HOUR, 8 * HOUR),
        ];
        let gaps = gaps(&refs(&rows));
        assert_eq!(
            gaps.spread,
            Some(Spread {
                min: 2 * HOUR,
                median: 2 * HOUR,
                max: 2 * HOUR
            })
        );
        assert_eq!(gaps.bins.iter().sum::<u32>(), 2);
    }

    #[test]
    fn statuses_count_in_their_order_and_confidence_spreads_over_ten_bins() {
        let mut rows = vec![row(0, 1), row(0, 1), row(0, 1), row(0, 1)];
        rows[0].status = Status::Final;
        rows[1].confidence = Some(0.0);
        rows[2].confidence = Some(1.0);
        rows[3].confidence = Some(0.55);
        rows[3].status = Status::Preliminary;
        let statuses = statuses(&refs(&rows));
        assert_eq!(
            statuses.counts,
            [
                (Status::Preliminary, 1),
                (Status::Confirmed, 2),
                (Status::Final, 1)
            ]
        );
        assert_eq!(statuses.without_confidence, 1);
        let mean = statuses.confidence_mean.unwrap();
        assert!((mean - 1.55 / 3.0).abs() < 1e-9, "{mean}");
        let mut bins = vec![0; CONFIDENCE_BINS];
        bins[0] = 1;
        bins[5] = 1;
        bins[9] = 1;
        assert_eq!(statuses.confidence_bins, bins);
    }

    #[test]
    fn numeric_attributes_give_their_range_and_median() {
        let rows = [
            with(json!({"density": 4})),
            with(json!({"density": 1.5})),
            with(json!({})),
            with(json!({"density": 10})),
            with(json!({"density": 2})),
        ];
        assert_eq!(
            attributes(&refs(&rows)),
            [AttributeStats {
                key: "density".into(),
                present: 4,
                values: Values::Numbers {
                    min: 1.5,
                    median: 3.0,
                    max: 10.0
                },
            }]
        );
    }

    #[test]
    fn text_and_mixed_attributes_give_their_most_frequent_values() {
        let rows = [
            with(json!({"spacecraft": "MMS1", "mixed": 1})),
            with(json!({"spacecraft": "MMS2", "mixed": "one"})),
            with(json!({"spacecraft": "MMS1", "mixed": true})),
            with(json!({"spacecraft": "MMS3"})),
            with(json!({"spacecraft": "MMS4"})),
        ];
        let stats = attributes(&refs(&rows));
        assert_eq!(stats[0].key, "mixed");
        assert_eq!(stats[0].present, 3);
        let Values::Text { distinct, .. } = &stats[0].values else {
            panic!("{:?}", stats[0]);
        };
        assert_eq!(*distinct, 3);
        assert_eq!(
            stats[1].values,
            Values::Text {
                distinct: 4,
                top: vec![share("MMS1", 2), share("MMS2", 1), share("MMS3", 1)],
            }
        );
    }

    #[test]
    fn authors_count_their_events_and_unknown_ones_apart() {
        let mut rows = vec![row(0, 1), row(0, 1), row(0, 1)];
        rows[0].author = Some("lavraud".into());
        rows[1].author = Some("lavraud".into());
        let authors = authors(&refs(&rows));
        assert_eq!(authors.shares, [share("lavraud", 2)]);
        assert_eq!(authors.missing, 1);
    }

    #[test]
    fn the_heading_counts_what_is_on_screen_out_of_the_whole() {
        assert_eq!(heading(412, 412), "412 events");
        assert_eq!(heading(1, 1), "1 event");
        assert_eq!(heading(37, 412), "37 of 412 events");
        assert_eq!(heading(0, 0), "No events");
    }

    #[test]
    fn percentages_keep_one_decimal_while_small() {
        assert_eq!(percent_text(0.0), "0 %");
        assert_eq!(percent_text(0.04), "< 0.1 %");
        assert_eq!(percent_text(1.234), "1.2 %");
        assert_eq!(percent_text(37.4), "37 %");
        assert_eq!(percent_text(100.0), "100 %");
    }

    #[test]
    fn numbers_drop_needless_decimals() {
        assert_eq!(number_text(3.0), "3");
        assert_eq!(number_text(1.25), "1.25");
        assert_eq!(number_text(1.0 / 3.0), "0.333");
        assert_eq!(number_text(-2.5), "-2.5");
        assert_eq!(number_text(-0.0001), "0");
    }
}
