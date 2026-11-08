//! Where things go on screen: the table's rendered window and the timeline's spans.

use std::ops::Range;

use crate::format::{date_text, minute_text};

/// Rows rendered beyond each edge of the view, so a scroll shows rows already there.
const OVERSCAN: usize = 10;
/// Over this long, the axis reads dates; under it, times.
const DATES_FROM: i64 = 4 * 86_400 * 1_000_000_000;

/// The rows to render for a scroll position, all rows being `row` pixels high.
pub fn window(scroll_top: f64, viewport: f64, row: f64, total: usize) -> Range<usize> {
    let first = (scroll_top / row).floor().max(0.0) as usize;
    let last = ((scroll_top + viewport) / row).ceil().max(0.0) as usize;
    let end = (last + OVERSCAN).min(total);
    first.saturating_sub(OVERSCAN).min(end)..end
}

/// A bar on the timeline, in percent of its width.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub left: f64,
    pub width: f64,
    pub lane: u8,
}

/// The timeline's bars for ranges in time order: events alternate between two lanes, and
/// a lane's events closer than one of `buckets` slices of the strip share a bar, so the
/// strip draws at most two bars per slice however many events there are.
pub fn spans(ranges: &[(i64, i64)], buckets: usize) -> Vec<Span> {
    let (Some(first), Some(last)) = (
        ranges.iter().map(|range| range.0).min(),
        ranges.iter().map(|range| range.1).max(),
    ) else {
        return Vec::new();
    };
    let length = (last - first).max(1) as f64;
    let bucket = |time: i64| (time - first) as f64 / length * buckets as f64;
    let mut lanes: [Vec<(usize, usize)>; 2] = [Vec::new(), Vec::new()];
    for (index, &(start, stop)) in ranges.iter().enumerate() {
        let from = (bucket(start).floor() as usize).min(buckets.saturating_sub(1));
        let to = (bucket(stop).ceil() as usize).max(from + 1);
        let lane = &mut lanes[index % 2];
        match lane.last_mut() {
            Some(bar) if from <= bar.1 => bar.1 = bar.1.max(to),
            _ => lane.push((from, to)),
        }
    }
    let percent = |slices: usize| slices as f64 * 100.0 / buckets as f64;
    let mut spans: Vec<Span> = (0..2u8)
        .flat_map(|lane| {
            lanes[usize::from(lane)]
                .iter()
                .map(move |&(from, to)| Span {
                    left: percent(from),
                    width: percent(to - from),
                    lane,
                })
        })
        .collect();
    spans.sort_by(|left, right| left.left.total_cmp(&right.left));
    spans
}

/// `count` labels spread evenly from `first` to `last`, both included.
pub fn ticks(first: i64, last: i64, count: usize) -> Vec<String> {
    let label = if last - first >= DATES_FROM {
        date_text
    } else {
        minute_text
    };
    let step = match count {
        0 | 1 => 0,
        _ => (last - first) / (count as i64 - 1),
    };
    (0..count as i64)
        .map(|tick| label(first + tick * step))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{DAY, DAY0};

    #[test]
    fn the_window_holds_the_rows_in_view_and_ten_on_each_side() {
        assert_eq!(window(0.0, 400.0, 40.0, 10_000), 0..20);
        assert_eq!(window(4_000.0, 400.0, 40.0, 10_000), 90..120);
        assert_eq!(window(3_600.0, 400.0, 40.0, 100), 80..100);
        assert_eq!(window(0.0, 400.0, 40.0, 0), 0..0);
    }

    #[test]
    fn events_far_apart_get_a_span_each_in_alternating_lanes() {
        let ranges = [(0, 10), (90, 100)];
        assert_eq!(
            spans(&ranges, 100),
            [
                Span {
                    left: 0.0,
                    width: 10.0,
                    lane: 0
                },
                Span {
                    left: 90.0,
                    width: 10.0,
                    lane: 1
                },
            ]
        );
    }

    #[test]
    fn events_closer_than_a_bucket_share_a_span() {
        let ranges: Vec<(i64, i64)> = (0..10_000).map(|i| (i, i + 1)).collect();
        let spans = spans(&ranges, 200);
        assert!(spans.len() <= 400, "{} spans", spans.len());
        assert_eq!(spans.iter().filter(|span| span.lane == 0).count(), 1);
    }

    #[test]
    fn a_point_event_still_shows() {
        let spans = spans(&[(0, 0), (50, 50), (100, 100)], 100);
        assert!(spans.iter().all(|span| span.width > 0.0));
    }

    #[test]
    fn the_axis_reads_dates_over_long_spans() {
        assert_eq!(
            ticks(DAY0 - DAY, DAY0 + 29 * DAY, 4),
            ["2019-11-02", "2019-11-12", "2019-11-22", "2019-12-02"]
        );
    }

    #[test]
    fn the_axis_reads_times_over_a_day() {
        assert_eq!(
            ticks(DAY0, DAY0 + DAY / 4, 2),
            ["2019-11-03 00:00", "2019-11-03 06:00"]
        );
    }
}
