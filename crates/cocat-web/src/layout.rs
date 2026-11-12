//! Where things go on screen: the table's rendered window, the sidebar's scroll to the
//! highlighted entry and the timeline's histogram.

use std::ops::Range;

use crate::format::{date_text, minute_text};
use crate::period::Extent;

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

/// A box's top and height on screen, in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub top: f64,
    pub height: f64,
}

/// Where a list scrolled to `scroll_top` and showing `list` on screen must scroll so that
/// `item` shows: nowhere when it already shows whole, else with the item in the middle.
pub fn reveal(scroll_top: f64, list: Span, item: Span) -> Option<f64> {
    let offset = item.top - list.top;
    if offset >= 0.0 && offset + item.height <= list.height {
        return None;
    }
    Some((scroll_top + offset - (list.height - item.height) / 2.0).max(0.0))
}

/// How many of `tags`, from the first, the table's tag cell shows in `width` pixels,
/// keeping room for the "+N" chip that counts the rest. At least one shows; the
/// stylesheet cuts it when it is too long.
pub fn tags_that_fit(tags: &[String], width: f64) -> usize {
    let mut used = 0.0;
    for (index, tag) in tags.iter().enumerate() {
        used += pill_width(tag) + TAG_GAP;
        let rest = if index + 1 < tags.len() {
            MORE_CHIP
        } else {
            0.0
        };
        if used + rest > width {
            return index.max(1);
        }
    }
    tags.len()
}

// simplify: a pill's width is estimated from its length in 12 px Inter, not measured; a
// ResizeObserver on the cell would be exact if the estimate shows wrong on some fonts.
const TAG_CHAR: f64 = 7.0;
const TAG_PADDING: f64 = 16.0;
const TAG_GAP: f64 = 4.0;
const MORE_CHIP: f64 = 34.0;

fn pill_width(tag: &str) -> f64 {
    tag.chars().count() as f64 * TAG_CHAR + TAG_PADDING
}

/// A bar of the timeline's histogram, in percent of the strip's width and height.
#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    pub left: f64,
    pub width: f64,
    pub height: f64,
}

/// How many of `ranges` overlap each of `buckets` equal slices of `extent`. An event
/// counts in every slice it touches, so an instant counts in one.
pub fn histogram(ranges: &[(i64, i64)], extent: &Extent, buckets: usize) -> Vec<u32> {
    if buckets == 0 {
        return Vec::new();
    }
    let length = (extent.last - extent.first).max(1) as f64;
    let slice = |time: i64| (time - extent.first) as f64 / length * buckets as f64;
    let mut steps = vec![0i64; buckets + 1];
    for &(start, stop) in ranges {
        let from = (slice(start).floor().max(0.0) as usize).min(buckets - 1);
        let to = (slice(stop).ceil() as usize).clamp(from + 1, buckets);
        steps[from] += 1;
        steps[to] -= 1;
    }
    steps[..buckets]
        .iter()
        .scan(0i64, |count, step| {
            *count += step;
            Some(u32::try_from(*count).unwrap_or(0))
        })
        .collect()
}

/// The histogram as bars: slices in a row with the same count share a bar, empty slices
/// draw nothing, and heights scale to the fullest slice with a floor so one event shows.
pub fn bars(counts: &[u32]) -> Vec<Bar> {
    let fullest = f64::from(counts.iter().copied().max().unwrap_or(0).max(1));
    let percent = |slices: usize| slices as f64 * 100.0 / counts.len() as f64;
    counts
        .chunk_by(|left, right| left == right)
        .scan(0, |from, run| {
            let left = *from;
            *from += run.len();
            Some((left, run))
        })
        .filter(|(_, run)| run[0] > 0)
        .map(|(from, run)| Bar {
            left: percent(from),
            width: percent(run.len()),
            height: (f64::from(run[0]) / fullest * 100.0).max(LOWEST_BAR),
        })
        .collect()
}

/// The shortest bar, in percent of the strip's height.
const LOWEST_BAR: f64 = 12.0;

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

/// A side pane the visitor can widen or narrow: the catalogue list on the left, the
/// statistics and event panel on the right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Sidebar,
    Panel,
}

impl Side {
    /// Narrowest and widest, in pixels.
    pub fn bounds(self) -> (f64, f64) {
        match self {
            Side::Sidebar => (180.0, 480.0),
            Side::Panel => (260.0, 640.0),
        }
    }

    pub fn default_width(self) -> f64 {
        match self {
            Side::Sidebar => 260.0,
            Side::Panel => 320.0,
        }
    }

    /// The CSS variable that sets its width, and where the width is remembered.
    pub fn key(self) -> &'static str {
        match self {
            Side::Sidebar => "--sidebar-width",
            Side::Panel => "--panel-width",
        }
    }

    pub fn clamp(self, width: f64) -> f64 {
        let (low, high) = self.bounds();
        width.clamp(low, high)
    }

    /// The width after the pointer moved `dx` pixels from where the drag started: the list's
    /// edge is on its right, the panel's on its left.
    pub fn dragged(self, start: f64, dx: f64) -> f64 {
        match self {
            Side::Sidebar => self.clamp(start + dx),
            Side::Panel => self.clamp(start - dx),
        }
    }

    /// The width after a key on the separator: the arrows move its edge by 16 px, Home goes
    /// back to the default; `None` for another key.
    pub fn keyed(self, width: f64, key: &str) -> Option<f64> {
        let step = match key {
            "ArrowLeft" => -16.0,
            "ArrowRight" => 16.0,
            "Home" => return Some(self.default_width()),
            _ => return None,
        };
        Some(self.dragged(width, step))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{DAY, DAY0};

    fn words(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    const LIST: Span = Span {
        top: 100.0,
        height: 400.0,
    };

    fn item(top: f64) -> Span {
        Span { top, height: 32.0 }
    }

    #[test]
    fn an_entry_already_in_view_does_not_move_the_list() {
        assert_eq!(reveal(0.0, LIST, item(100.0)), None);
        assert_eq!(reveal(250.0, LIST, item(468.0)), None);
    }

    #[test]
    fn an_entry_out_of_view_is_scrolled_to_the_list_s_middle() {
        assert_eq!(reveal(0.0, LIST, item(3_000.0)), Some(2_716.0));
        assert_eq!(reveal(2_716.0, LIST, item(80.0)), Some(2_512.0));
    }

    #[test]
    fn an_entry_near_the_top_scrolls_no_further_than_the_start() {
        assert_eq!(reveal(50.0, LIST, item(60.0)), Some(0.0));
    }

    #[test]
    fn every_tag_shows_when_they_all_fit() {
        assert_eq!(tags_that_fit(&words(&["sw", "mms1"]), 160.0), 2);
        assert_eq!(tags_that_fit(&[], 160.0), 0);
    }

    #[test]
    fn tags_that_do_not_fit_leave_room_for_the_count_of_the_rest() {
        let tags = words(&["bow shock", "by hand", "checked", "inbound"]);
        let shown = tags_that_fit(&tags, 160.0);
        assert!((1..tags.len()).contains(&shown), "{shown}");
        let used: f64 = tags[..shown]
            .iter()
            .map(|tag| pill_width(tag) + TAG_GAP)
            .sum();
        assert!(used + MORE_CHIP <= 160.0, "{used}");
    }

    #[test]
    fn one_tag_always_shows_even_when_too_long() {
        let tags = words(&["a very long tag name that cannot fit", "b"]);
        assert_eq!(tags_that_fit(&tags, 160.0), 1);
    }

    #[test]
    fn the_window_holds_the_rows_in_view_and_ten_on_each_side() {
        assert_eq!(window(0.0, 400.0, 40.0, 10_000), 0..20);
        assert_eq!(window(4_000.0, 400.0, 40.0, 10_000), 90..120);
        assert_eq!(window(3_600.0, 400.0, 40.0, 100), 80..100);
        assert_eq!(window(0.0, 400.0, 40.0, 0), 0..0);
    }

    const YEAR: Extent = Extent {
        first: 0,
        last: 100,
    };

    #[test]
    fn each_slice_counts_the_events_that_overlap_it() {
        let counts = histogram(&[(0, 10), (5, 30), (90, 100)], &YEAR, 10);
        assert_eq!(counts, [2, 1, 1, 0, 0, 0, 0, 0, 0, 1]);
    }

    #[test]
    fn an_instant_counts_once_even_at_the_strip_s_end() {
        assert_eq!(histogram(&[(55, 55)], &YEAR, 10)[5], 1);
        assert_eq!(histogram(&[(100, 100)], &YEAR, 10)[9], 1);
        assert_eq!(histogram(&[(55, 55)], &YEAR, 10).iter().sum::<u32>(), 1);
    }

    #[test]
    fn a_strip_of_one_instant_counts_every_event() {
        let instant = Extent { first: 7, last: 7 };
        assert_eq!(histogram(&[(7, 7), (7, 7)], &instant, 4), [2, 0, 0, 0]);
    }

    #[test]
    fn many_events_cost_one_pass() {
        let ranges: Vec<(i64, i64)> = (0..100_000).map(|i| (i, i + 50_000)).collect();
        let extent = Extent {
            first: 0,
            last: 150_000,
        };
        let counts = histogram(&ranges, &extent, 400);
        assert_eq!(counts.len(), 400);
        let fullest = counts.iter().copied().max().unwrap_or(0);
        assert!((50_000..=50_375).contains(&fullest), "{fullest}");
    }

    #[test]
    fn bars_scale_to_the_fullest_slice_and_merge_equal_neighbours() {
        assert_eq!(
            bars(&[4, 4, 0, 2]),
            [
                Bar {
                    left: 0.0,
                    width: 50.0,
                    height: 100.0
                },
                Bar {
                    left: 75.0,
                    width: 25.0,
                    height: 50.0
                },
            ]
        );
    }

    #[test]
    fn a_lone_event_beside_a_crowd_still_shows() {
        let bars = bars(&[1000, 0, 1]);
        assert_eq!(bars[1].height, LOWEST_BAR);
    }

    #[test]
    fn no_events_draw_no_bars() {
        assert!(bars(&[0, 0, 0]).is_empty());
        assert!(bars(&[]).is_empty());
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

    #[test]
    fn a_side_pane_stays_within_its_bounds_and_steps_by_key() {
        assert_eq!(Side::Sidebar.clamp(50.0), Side::Sidebar.bounds().0);
        assert_eq!(Side::Panel.clamp(5_000.0), Side::Panel.bounds().1);
        assert_eq!(Side::Panel.clamp(400.0), 400.0);
        assert_eq!(
            Side::Sidebar.dragged(260.0, 40.0),
            300.0,
            "the list grows to the right"
        );
        assert_eq!(
            Side::Panel.dragged(320.0, 40.0),
            280.0,
            "the panel grows to the left"
        );
        assert_eq!(Side::Panel.keyed(320.0, "ArrowLeft"), Some(336.0));
        assert_eq!(Side::Sidebar.keyed(260.0, "ArrowLeft"), Some(244.0));
        assert_eq!(
            Side::Sidebar.keyed(400.0, "Home"),
            Some(Side::Sidebar.default_width())
        );
        assert_eq!(Side::Sidebar.keyed(260.0, "a"), None);
    }
}
