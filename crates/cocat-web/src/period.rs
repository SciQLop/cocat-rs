//! A period picked on the timeline: where a pointer on the strip falls in time, how a drag
//! makes or changes the period, and which events it keeps.

use crate::format::{date_text, minute_text};

/// Under this long, the period reads with its times, not just its dates.
const TIMES_UNDER: i64 = 2 * 86_400 * 1_000_000_000;
/// Two presses closer than this, in time and pixels, are a double tap.
const DOUBLE_TAP_MS: f64 = 400.0;
const DOUBLE_TAP_PX: f64 = 24.0;

/// The time the strip covers, from its left edge to its right edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Extent {
    pub first: i64,
    pub last: i64,
}

impl Extent {
    /// From the earliest start to the latest stop; none without ranges.
    pub fn of(ranges: &[(i64, i64)]) -> Option<Self> {
        let first = ranges.iter().map(|range| range.0).min()?;
        let last = ranges.iter().map(|range| range.1).max()?;
        Some(Self { first, last })
    }

    /// The time under pixel `x` of a strip `width` pixels wide, clamped to the strip.
    pub fn time_at(&self, x: f64, width: f64) -> i64 {
        let fraction = if width > 0.0 {
            (x / width).clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.first + (fraction * (self.last - self.first) as f64).round() as i64
    }

    /// Where `time` lies on the strip, in percent of its width, clamped to the strip.
    pub fn percent_of(&self, time: i64) -> f64 {
        ((time - self.first) as f64 / self.length() as f64 * 100.0).clamp(0.0, 100.0)
    }

    /// The pixel of `time` on a strip `width` pixels wide.
    pub fn pixel_of(&self, time: i64, width: f64) -> f64 {
        self.percent_of(time) / 100.0 * width
    }

    fn length(&self) -> i64 {
        (self.last - self.first).max(1)
    }
}

/// A period of time, start before stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Period {
    pub start: i64,
    pub stop: i64,
}

impl Period {
    /// The period between two times, in either order.
    pub fn between(one: i64, other: i64) -> Self {
        Self {
            start: one.min(other),
            stop: one.max(other),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.stop
    }

    /// An event overlaps when it starts before the period ends and stops after it starts.
    pub fn overlaps(&self, start: i64, stop: i64) -> bool {
        start < self.stop && stop > self.start
    }

    /// "2019-11-03 → 2019-11-09", with the times when it is shorter than two days.
    pub fn text(&self) -> String {
        let (start, stop) = self.ends();
        format!("{start} → {stop}")
    }

    fn ends(&self) -> (String, String) {
        let label = if self.stop - self.start < TIMES_UNDER {
            minute_text
        } else {
            date_text
        };
        (label(self.start), label(self.stop))
    }
}

/// "Timeline: 412 events from 2019-11-03 to 2019-11-09", for those who do not see it.
pub fn strip_label(count: usize, extent: Option<Extent>) -> String {
    let Some(extent) = extent else {
        return "Timeline: no events".to_owned();
    };
    let (first, last) = Period::between(extent.first, extent.last).ends();
    let plural = if count == 1 { "" } else { "s" };
    format!("Timeline: {count} event{plural} from {first} to {last}")
}

/// "2019-11-03 → 2019-11-09 · 37 of 412 events".
pub fn counts_text(period: &Period, shown: usize, total: usize) -> String {
    let plural = if total == 1 { "" } else { "s" };
    format!("{} · {shown} of {total} event{plural}", period.text())
}

/// What a press on the strip holds while the pointer moves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Grip {
    /// One end stays at this time and the other follows the pointer: a new period, or a
    /// handle dragged.
    Anchored(i64),
    /// The whole period follows the pointer from where it was grabbed.
    Moving { grab: i64, period: Period },
}

impl Grip {
    /// What a press at pixel `x` grabs: a handle of the period within `reach` pixels of it,
    /// the period itself inside it, else the start of a new one.
    pub fn at(period: Option<Period>, extent: &Extent, x: f64, width: f64, reach: f64) -> Self {
        let time = extent.time_at(x, width);
        let Some(period) = period else {
            return Self::Anchored(time);
        };
        let start = extent.pixel_of(period.start, width);
        let stop = extent.pixel_of(period.stop, width);
        if (x - stop).abs() <= reach {
            Self::Anchored(period.start)
        } else if (x - start).abs() <= reach {
            Self::Anchored(period.stop)
        } else if (start..stop).contains(&x) {
            Self::Moving { grab: time, period }
        } else {
            Self::Anchored(time)
        }
    }

    /// The period with the pointer at `time`; a moved period keeps its length and stays on
    /// the strip.
    pub fn drag(&self, time: i64, extent: &Extent) -> Period {
        match *self {
            Self::Anchored(anchor) => Period::between(anchor, time),
            Self::Moving { grab, period } => {
                let length = period.stop - period.start;
                let latest = (extent.last - length).max(extent.first);
                let start = (period.start + time - grab).clamp(extent.first, latest);
                Period {
                    start,
                    stop: start + length,
                }
            }
        }
    }
}

/// A press on the strip: when, in milliseconds, and where, in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tap {
    pub at: f64,
    pub x: f64,
}

impl Tap {
    pub fn doubles(&self, previous: Option<Tap>) -> bool {
        previous.is_some_and(|previous| {
            self.at - previous.at < DOUBLE_TAP_MS && (self.x - previous.x).abs() < DOUBLE_TAP_PX
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{DAY, DAY0, SECOND};

    const EXTENT: Extent = Extent {
        first: 0,
        last: 1000,
    };

    #[test]
    fn the_extent_runs_from_the_first_start_to_the_last_stop() {
        assert_eq!(
            Extent::of(&[(30, 40), (10, 20), (35, 90)]),
            Some(Extent {
                first: 10,
                last: 90
            })
        );
        assert_eq!(Extent::of(&[]), None);
    }

    #[test]
    fn pixels_map_to_times_and_back() {
        assert_eq!(EXTENT.time_at(0.0, 200.0), 0);
        assert_eq!(EXTENT.time_at(50.0, 200.0), 250);
        assert_eq!(EXTENT.time_at(200.0, 200.0), 1000);
        assert_eq!(EXTENT.pixel_of(250, 200.0), 50.0);
        assert_eq!(EXTENT.percent_of(750), 75.0);
    }

    #[test]
    fn a_pointer_beyond_the_strip_counts_as_its_edge() {
        assert_eq!(EXTENT.time_at(-40.0, 200.0), 0);
        assert_eq!(EXTENT.time_at(260.0, 200.0), 1000);
        assert_eq!(EXTENT.percent_of(-500), 0.0);
        assert_eq!(EXTENT.percent_of(5000), 100.0);
    }

    #[test]
    fn a_strip_of_one_instant_or_no_width_does_not_divide_by_zero() {
        let instant = Extent {
            first: 50,
            last: 50,
        };
        assert_eq!(instant.time_at(100.0, 200.0), 50);
        assert_eq!(instant.percent_of(50), 0.0);
        assert_eq!(EXTENT.time_at(10.0, 0.0), 0);
    }

    #[test]
    fn a_period_is_the_same_dragged_either_way() {
        assert_eq!(Period::between(700, 200), Period::between(200, 700));
        assert_eq!(
            Period::between(700, 200),
            Period {
                start: 200,
                stop: 700
            }
        );
    }

    #[test]
    fn a_drag_from_an_anchor_follows_the_pointer_past_it() {
        let grip = Grip::Anchored(400);
        assert_eq!(grip.drag(600, &EXTENT), Period::between(400, 600));
        assert_eq!(grip.drag(100, &EXTENT), Period::between(100, 400));
        assert!(grip.drag(400, &EXTENT).is_empty());
    }

    #[test]
    fn a_press_grabs_a_handle_the_band_or_starts_anew() {
        let period = Some(Period::between(250, 500));
        let at = |x| Grip::at(period, &EXTENT, x, 200.0, 8.0);
        assert_eq!(at(52.0), Grip::Anchored(500), "start handle");
        assert_eq!(at(97.0), Grip::Anchored(250), "stop handle");
        assert_eq!(
            at(75.0),
            Grip::Moving {
                grab: 375,
                period: Period::between(250, 500)
            }
        );
        assert_eq!(at(150.0), Grip::Anchored(750), "outside");
        assert_eq!(
            Grip::at(None, &EXTENT, 75.0, 200.0, 8.0),
            Grip::Anchored(375)
        );
    }

    #[test]
    fn the_handles_of_a_narrow_period_stay_apart_from_moving_it() {
        let period = Some(Period::between(500, 505));
        let grip = Grip::at(period, &EXTENT, 101.0, 200.0, 8.0);
        assert_eq!(grip, Grip::Anchored(500), "the stop handle wins");
    }

    #[test]
    fn a_moved_period_keeps_its_length_and_stays_on_the_strip() {
        let grip = Grip::Moving {
            grab: 300,
            period: Period::between(200, 400),
        };
        assert_eq!(grip.drag(350, &EXTENT), Period::between(250, 450));
        assert_eq!(grip.drag(0, &EXTENT), Period::between(0, 200));
        assert_eq!(grip.drag(1000, &EXTENT), Period::between(800, 1000));
    }

    #[test]
    fn a_period_longer_than_the_strip_moves_to_its_start() {
        let grip = Grip::Moving {
            grab: 0,
            period: Period::between(-100, 2000),
        };
        assert_eq!(grip.drag(500, &EXTENT), Period::between(0, 2100));
    }

    #[test]
    fn events_overlapping_the_period_are_kept() {
        let period = Period::between(100, 200);
        assert!(period.overlaps(150, 160), "inside");
        assert!(period.overlaps(50, 150), "across the start");
        assert!(period.overlaps(150, 250), "across the stop");
        assert!(period.overlaps(50, 250), "around");
        assert!(period.overlaps(150, 150), "an instant inside");
    }

    #[test]
    fn events_that_only_touch_the_period_are_not() {
        let period = Period::between(100, 200);
        assert!(!period.overlaps(50, 100));
        assert!(!period.overlaps(200, 250));
        assert!(!period.overlaps(100, 100));
        assert!(!period.overlaps(0, 10));
    }

    #[test]
    fn a_long_period_reads_in_dates_a_short_one_with_times() {
        let long = Period::between(DAY0, DAY0 + 6 * DAY);
        assert_eq!(long.text(), "2019-11-03 → 2019-11-09");
        let short = Period::between(DAY0 + 3600 * SECOND, DAY0 + DAY);
        assert_eq!(short.text(), "2019-11-03 01:00 → 2019-11-04 00:00");
    }

    #[test]
    fn the_counts_say_how_many_of_how_many() {
        let period = Period::between(DAY0, DAY0 + 6 * DAY);
        assert_eq!(
            counts_text(&period, 37, 412),
            "2019-11-03 → 2019-11-09 · 37 of 412 events"
        );
        assert_eq!(
            counts_text(&period, 1, 1),
            "2019-11-03 → 2019-11-09 · 1 of 1 event"
        );
    }

    #[test]
    fn the_strip_says_how_many_events_it_spans_and_when() {
        let extent = Extent {
            first: DAY0,
            last: DAY0 + 6 * DAY,
        };
        assert_eq!(
            strip_label(412, Some(extent)),
            "Timeline: 412 events from 2019-11-03 to 2019-11-09"
        );
        let short = Extent {
            first: DAY0,
            last: DAY0 + 3600 * SECOND,
        };
        assert_eq!(
            strip_label(1, Some(short)),
            "Timeline: 1 event from 2019-11-03 00:00 to 2019-11-03 01:00"
        );
        assert_eq!(strip_label(0, None), "Timeline: no events");
    }

    #[test]
    fn two_close_presses_in_quick_succession_are_a_double_tap() {
        let tap = |at, x| Tap { at, x };
        let first = tap(1000.0, 50.0);
        assert!(tap(1250.0, 60.0).doubles(Some(first)));
        assert!(!tap(1500.0, 50.0).doubles(Some(first)), "too late");
        assert!(!tap(1100.0, 120.0).doubles(Some(first)), "too far");
        assert!(!first.doubles(None));
    }
}
