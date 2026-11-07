//! Interval operators on half-open intervals `[start, stop)`; a zero-length item is a point
//! `t`, inside an interval when `start <= t < stop` (design section 3).

use cocat_core::rules::RuleError;
use cocat_core::rules::intervals::{
    Item, closest, intersect, merge, seconds, shift, subtract, tile, union, window,
};

fn event(uuid: &str, start: i64, stop: i64) -> Item {
    Item::event(uuid, start, stop)
}

fn derived(start: i64, stop: i64, sources: &[&str]) -> Item {
    Item::derived(
        start,
        stop,
        sources.iter().map(|s| (*s).to_owned()).collect(),
    )
}

fn with_gap(mut item: Item, gap: Option<i64>) -> Item {
    item.gap = gap;
    item
}

#[test]
fn seconds_become_nanoseconds() {
    assert_eq!(seconds(1.5), 1_500_000_000);
    assert_eq!(seconds(-2.0), -2_000_000_000);
}

#[test]
fn merge_of_nothing_is_nothing() {
    assert_eq!(merge(&[], 0), vec![]);
}

#[test]
fn merge_joins_overlapping_intervals() {
    let merged = merge(&[event("a", 0, 10), event("b", 5, 15)], 0);
    assert_eq!(merged, vec![derived(0, 15, &["a", "b"])]);
}

#[test]
fn merge_joins_touching_intervals() {
    let merged = merge(&[event("b", 10, 20), event("a", 0, 10)], 0);
    assert_eq!(merged, vec![derived(0, 20, &["a", "b"])]);
}

#[test]
fn merge_absorbs_nested_intervals() {
    let merged = merge(
        &[event("a", 0, 100), event("b", 10, 20), event("c", 30, 40)],
        0,
    );
    assert_eq!(merged, vec![derived(0, 100, &["a", "b", "c"])]);
}

#[test]
fn merge_of_identical_intervals_is_one() {
    let merged = merge(&[event("a", 0, 10), event("b", 0, 10)], 0);
    assert_eq!(merged, vec![derived(0, 10, &["a", "b"])]);
}

#[test]
fn merge_keeps_apart_what_a_zero_gap_separates() {
    let merged = merge(&[event("a", 0, 10), event("b", 11, 20)], 0);
    assert_eq!(
        merged,
        vec![derived(0, 10, &["a"]), derived(11, 20, &["b"])]
    );
}

#[test]
fn merge_joins_intervals_up_to_the_gap() {
    let items = [event("a", 0, 10), event("b", 15, 20), event("c", 26, 30)];
    assert_eq!(
        merge(&items, 5),
        vec![derived(0, 20, &["a", "b"]), derived(26, 30, &["c"])]
    );
}

#[test]
fn merge_absorbs_a_point_inside_and_one_touching_the_stop() {
    let merged = merge(
        &[event("a", 0, 10), event("p", 5, 5), event("q", 10, 10)],
        0,
    );
    assert_eq!(merged, vec![derived(0, 10, &["a", "p", "q"])]);
}

#[test]
fn merge_of_identical_points_is_one_point() {
    let merged = merge(&[event("p", 7, 7), event("q", 7, 7), event("r", 8, 8)], 0);
    assert_eq!(
        merged,
        vec![derived(7, 7, &["p", "q"]), derived(8, 8, &["r"])]
    );
}

#[test]
fn merge_carries_the_sources_of_derived_intervals() {
    let merged = merge(&[derived(0, 10, &["a", "b"]), event("c", 5, 12)], 0);
    assert_eq!(merged, vec![derived(0, 12, &["a", "b", "c"])]);
}

#[test]
fn intersect_with_nothing_is_nothing() {
    assert_eq!(intersect(&[event("a", 0, 10)], &[]), vec![]);
    assert_eq!(intersect(&[], &[event("a", 0, 10)]), vec![]);
}

#[test]
fn intersect_of_overlapping_intervals_is_the_overlap() {
    let both = intersect(&[event("a", 0, 10)], &[event("b", 5, 15)]);
    assert_eq!(both, vec![derived(5, 10, &["a", "b"])]);
}

#[test]
fn intersect_of_touching_intervals_is_nothing() {
    assert_eq!(
        intersect(&[event("a", 0, 10)], &[event("b", 10, 20)]),
        vec![]
    );
}

#[test]
fn intersect_of_nested_intervals_is_the_inner_one() {
    let both = intersect(&[event("a", 0, 100)], &[event("b", 10, 20)]);
    assert_eq!(both, vec![derived(10, 20, &["a", "b"])]);
}

#[test]
fn intersect_of_identical_intervals_is_that_interval() {
    let both = intersect(&[event("a", 0, 10)], &[event("b", 0, 10)]);
    assert_eq!(both, vec![derived(0, 10, &["a", "b"])]);
}

#[test]
fn intersect_keeps_a_point_from_its_start_up_to_before_its_stop() {
    let points = [event("p", 0, 0), event("q", 5, 5), event("r", 10, 10)];
    let both = intersect(&points, &[event("b", 0, 10)]);
    assert_eq!(
        both,
        vec![derived(0, 0, &["b", "p"]), derived(5, 5, &["b", "q"])]
    );
}

#[test]
fn intersect_of_identical_points_is_the_point() {
    let both = intersect(&[event("p", 5, 5)], &[event("q", 5, 5), event("r", 6, 6)]);
    assert_eq!(both, vec![derived(5, 5, &["p", "q"])]);
}

#[test]
fn intersect_finds_a_long_interval_that_starts_far_before() {
    let b = [event("long", 0, 1_000), event("short", 990, 995)];
    let both = intersect(&[event("a", 900, 910)], &b);
    assert_eq!(both, vec![derived(900, 910, &["a", "long"])]);
}

#[test]
fn intersect_pairs_every_overlap() {
    let both = intersect(
        &[event("a", 0, 10), event("c", 8, 30)],
        &[event("b", 5, 20)],
    );
    assert_eq!(
        both,
        vec![derived(5, 10, &["a", "b"]), derived(8, 20, &["b", "c"])]
    );
}

#[test]
fn subtract_nothing_keeps_every_item_as_it_is() {
    let a = [event("a", 0, 10), derived(20, 30, &["x"])];
    assert_eq!(subtract(&a, &[]), a.to_vec());
}

#[test]
fn subtract_from_nothing_is_nothing() {
    assert_eq!(subtract(&[], &[event("b", 0, 10)]), vec![]);
}

#[test]
fn subtract_leaves_a_touching_interval_untouched() {
    let left = subtract(&[event("a", 0, 10)], &[event("b", 10, 20)]);
    assert_eq!(left, vec![event("a", 0, 10)]);
}

#[test]
fn subtract_of_a_nested_interval_splits_in_two() {
    let left = subtract(
        &[event("a", 0, 100)],
        &[event("b", 10, 20), event("c", 15, 30)],
    );
    assert_eq!(left, vec![derived(0, 10, &["a"]), derived(30, 100, &["a"])]);
}

#[test]
fn subtract_of_an_identical_interval_is_nothing() {
    assert_eq!(subtract(&[event("a", 0, 10)], &[event("b", 0, 10)]), vec![]);
}

#[test]
fn subtract_cuts_an_overlap_off_one_side() {
    let left = subtract(&[event("a", 0, 10)], &[event("b", 5, 15)]);
    assert_eq!(left, vec![derived(0, 5, &["a"])]);
}

#[test]
fn subtract_removes_a_point_inside_but_not_at_the_stop() {
    let points = [event("p", 0, 0), event("q", 10, 10)];
    let left = subtract(&points, &[event("b", 0, 10)]);
    assert_eq!(left, vec![event("q", 10, 10)]);
}

#[test]
fn subtract_removes_a_point_equal_to_a_point() {
    let left = subtract(&[event("p", 5, 5), event("q", 6, 6)], &[event("r", 5, 5)]);
    assert_eq!(left, vec![event("q", 6, 6)]);
}

#[test]
fn subtract_of_a_point_leaves_an_interval_whole() {
    let left = subtract(&[event("a", 0, 10)], &[event("p", 5, 5)]);
    assert_eq!(left, vec![event("a", 0, 10)]);
}

#[test]
fn shift_moves_items_back_with_a_negative_offset() {
    let moved = shift(&[event("a", 100, 200)], -150);
    assert_eq!(moved, vec![derived(-50, 50, &["a"])]);
}

#[test]
fn shift_saturates_at_the_end_of_time() {
    let moved = shift(&[event("a", i64::MAX - 10, i64::MAX)], 100);
    assert_eq!(moved, vec![derived(i64::MAX, i64::MAX, &["a"])]);
}

#[test]
fn tile_cuts_an_interval_into_windows_and_truncates_the_last() {
    let tiles = tile(&[event("a", 0, 25)], 10).unwrap();
    assert_eq!(
        tiles,
        vec![
            derived(0, 10, &["a"]),
            derived(10, 20, &["a"]),
            derived(20, 25, &["a"])
        ]
    );
}

#[test]
fn tile_of_an_exact_multiple_has_no_empty_window() {
    let tiles = tile(&[event("a", 0, 20)], 10).unwrap();
    assert_eq!(tiles, vec![derived(0, 10, &["a"]), derived(10, 20, &["a"])]);
}

#[test]
fn tile_keeps_a_point_as_a_point() {
    assert_eq!(
        tile(&[event("p", 5, 5)], 10).unwrap(),
        vec![derived(5, 5, &["p"])]
    );
}

#[test]
fn tile_refuses_to_make_too_many_windows() {
    let error = tile(&[event("a", 0, i64::MAX)], 1).unwrap_err();
    assert!(matches!(error, RuleError::TooManyTiles));
}

#[test]
fn window_keeps_items_touching_with_a_zero_within() {
    let a = [event("a", 0, 10), event("far", 12, 13)];
    assert_eq!(
        window(&a, &[event("b", 10, 11)], 0),
        vec![event("a", 0, 10)]
    );
}

#[test]
fn window_keeps_items_up_to_within_on_either_side() {
    let a = [
        event("left", 0, 5),
        event("too-far", 0, 4),
        event("right", 25, 30),
    ];
    let kept = window(&a, &[event("b", 10, 20)], 5);
    assert_eq!(kept, vec![event("left", 0, 5), event("right", 25, 30)]);
}

#[test]
fn window_against_nothing_keeps_nothing() {
    assert_eq!(window(&[event("a", 0, 10)], &[], 1_000), vec![]);
}

#[test]
fn closest_gives_each_item_its_gap_to_the_nearest() {
    let a = [
        event("over", 5, 15),
        event("near", 32, 33),
        event("point", 21, 21),
    ];
    let b = [event("b", 10, 20), event("c", 35, 40)];
    assert_eq!(
        closest(&a, &b),
        vec![
            with_gap(event("over", 5, 15), Some(0)),
            with_gap(event("point", 21, 21), Some(1)),
            with_gap(event("near", 32, 33), Some(2)),
        ]
    );
}

#[test]
fn closest_counts_touching_as_no_gap() {
    let got = closest(&[event("a", 0, 10)], &[event("b", 10, 20)]);
    assert_eq!(got, vec![with_gap(event("a", 0, 10), Some(0))]);
}

#[test]
fn closest_to_nothing_has_no_gap() {
    let got = closest(&[event("a", 0, 10)], &[]);
    assert_eq!(got, vec![event("a", 0, 10)]);
}

#[test]
fn union_keeps_each_event_once_and_sorts() {
    let got = union(&[
        vec![event("b", 5, 6), event("a", 0, 1)],
        vec![event("a", 0, 1), derived(2, 3, &["x"])],
        vec![],
    ]);
    assert_eq!(
        got,
        vec![event("a", 0, 1), derived(2, 3, &["x"]), event("b", 5, 6)]
    );
}

#[test]
fn union_keeps_the_gap_closest_gave() {
    let got = union(&[
        vec![event("a", 0, 1)],
        vec![with_gap(event("a", 0, 1), Some(3))],
    ]);
    assert_eq!(got, vec![with_gap(event("a", 0, 1), Some(3))]);
}
