//! How the review and decide pages move through the rows the shared review rules make: the
//! keys, the filter, the selection, the columns. The rules themselves live in
//! `cocat_core::round::review`.

use std::collections::BTreeSet;

use cocat_core::round::review::{ReviewRow, Verdict, next_unreviewed, progress};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Verdict(Verdict),
    Next,
    Previous,
}

const KEYS: [(&str, KeyAction); 5] = [
    ("a", KeyAction::Verdict(Verdict::Accept)),
    ("r", KeyAction::Verdict(Verdict::Reject)),
    ("u", KeyAction::Verdict(Verdict::Unsure)),
    ("j", KeyAction::Next),
    ("k", KeyAction::Previous),
];

/// What a key does on the review page; none for any other key.
pub fn key_action(key: &str) -> Option<KeyAction> {
    KEYS.iter()
        .find(|(name, _)| *name == key)
        .map(|(_, action)| *action)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    NotReviewed,
    All,
}

/// The rows the table shows, by their index: the current ones, all or the unreviewed.
pub fn shown(rows: &[ReviewRow], filter: Filter) -> Vec<usize> {
    let wanted = |row: &ReviewRow| filter == Filter::All || row.mine.is_none();
    rows.iter()
        .enumerate()
        .filter(|(_, row)| !row.set_aside && wanted(row))
        .map(|(index, _)| index)
        .collect()
}

/// The row picked: the one the URL names when shown, else the first unreviewed, else the
/// first shown.
pub fn selected(rows: &[ReviewRow], shown: &[usize], named: Option<&str>) -> Option<usize> {
    let named = shown
        .iter()
        .copied()
        .find(|&index| Some(rows[index].event.uuid.as_str()) == named);
    named
        .or_else(|| next_unreviewed(rows, 0))
        .or_else(|| shown.first().copied())
}

/// The shown row after or before `current`, staying put at either end.
pub fn step(shown: &[usize], current: Option<usize>, forward: bool) -> Option<usize> {
    let Some(at) = current.and_then(|current| shown.iter().position(|&i| i == current)) else {
        return shown.first().copied();
    };
    let next = if forward {
        (at + 1).min(shown.len() - 1)
    } else {
        at.saturating_sub(1)
    };
    Some(shown[next])
}

/// The row to pick once `current` got a verdict: the next unreviewed, else `current`.
pub fn after_verdict(rows: &[ReviewRow], current: usize) -> usize {
    next_unreviewed(rows, current + 1).unwrap_or(current)
}

/// A verdict as the table says it.
pub fn verdict_word(verdict: Option<Verdict>) -> &'static str {
    match verdict {
        Some(Verdict::Accept) => "accepted",
        Some(Verdict::Reject) => "rejected",
        Some(Verdict::Unsure) => "unsure",
        None => "",
    }
}

/// The visitor's verdict on a row; one from an earlier revision says so.
pub fn mine_text(row: &ReviewRow) -> String {
    match (row.mine, row.previous) {
        (None, Some(previous)) => {
            format!(
                "{} (from the previous review)",
                verdict_word(Some(previous))
            )
        }
        (mine, _) => verdict_word(mine).to_owned(),
    }
}

/// "{done} of {total} reviewed".
pub fn counter(rows: &[ReviewRow]) -> String {
    let (done, total, _) = progress(rows);
    format!("{done} of {total} reviewed")
}

/// The current rows for the decider, the most accepted first.
pub fn by_accepts(rows: &[ReviewRow]) -> Vec<usize> {
    let mut order = shown(rows, Filter::All);
    order.sort_by_key(|&index| std::cmp::Reverse(rows[index].accepts));
    order
}

/// The attribute columns: the budget's first, then the others by name.
pub fn attribute_columns(rows: &[ReviewRow], budget: Option<&str>) -> Vec<String> {
    let others: BTreeSet<&String> = rows
        .iter()
        .flat_map(|row| row.event.attributes.keys())
        .filter(|key| Some(key.as_str()) != budget)
        .collect();
    budget
        .map(str::to_owned)
        .into_iter()
        .chain(others.into_iter().cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cocat_core::model::EventRecord;
    use cocat_core::time::Timestamp;
    use serde_json::json;

    fn row(uuid: &str, accepts: u32, mine: Option<Verdict>) -> ReviewRow {
        let at = Timestamp::parse("2020-01-01T00:00:00Z").unwrap();
        ReviewRow {
            event: EventRecord::new(uuid, at, at),
            proposal: None,
            accepts,
            rejects: 0,
            unsure: 0,
            mine,
            previous: None,
            set_aside: false,
        }
    }

    fn sample() -> Vec<ReviewRow> {
        let mut gone = row("gone", 3, None);
        gone.set_aside = true;
        vec![
            row("a", 1, Some(Verdict::Accept)),
            row("b", 2, None),
            row("c", 0, Some(Verdict::Reject)),
            row("d", 2, None),
            gone,
        ]
    }

    #[test]
    fn a_key_press_maps_to_a_verdict() {
        assert_eq!(key_action("a"), Some(KeyAction::Verdict(Verdict::Accept)));
        assert_eq!(key_action("r"), Some(KeyAction::Verdict(Verdict::Reject)));
        assert_eq!(key_action("u"), Some(KeyAction::Verdict(Verdict::Unsure)));
        assert_eq!(key_action("j"), Some(KeyAction::Next));
        assert_eq!(key_action("k"), Some(KeyAction::Previous));
        assert_eq!(key_action("A"), None);
        assert_eq!(key_action("x"), None);
    }

    #[test]
    fn the_filter_shows_current_rows_all_or_unreviewed() {
        assert_eq!(shown(&sample(), Filter::All), [0, 1, 2, 3]);
        assert_eq!(shown(&sample(), Filter::NotReviewed), [1, 3]);
    }

    #[test]
    fn the_selection_follows_the_url_else_the_first_unreviewed() {
        let rows = sample();
        let all = shown(&rows, Filter::All);
        assert_eq!(selected(&rows, &all, Some("c")), Some(2));
        assert_eq!(selected(&rows, &all, None), Some(1));
        assert_eq!(selected(&rows, &all, Some("gone")), Some(1));
        let unreviewed = shown(&rows, Filter::NotReviewed);
        assert_eq!(selected(&rows, &unreviewed, Some("a")), Some(1));
        assert_eq!(selected(&rows[..1], &[0], None), Some(0));
        assert_eq!(selected(&[], &[], None), None);
    }

    #[test]
    fn j_and_k_move_through_the_shown_rows_and_stop_at_the_ends() {
        let shown = [1, 3, 4];
        assert_eq!(step(&shown, Some(1), true), Some(3));
        assert_eq!(step(&shown, Some(4), true), Some(4));
        assert_eq!(step(&shown, Some(3), false), Some(1));
        assert_eq!(step(&shown, Some(1), false), Some(1));
        assert_eq!(step(&shown, None, true), Some(1));
        assert_eq!(step(&[], None, true), None);
    }

    #[test]
    fn a_verdict_moves_to_the_next_unreviewed_row() {
        let rows = sample();
        assert_eq!(after_verdict(&rows, 1), 3);
        assert_eq!(after_verdict(&rows, 3), 1);
        assert_eq!(after_verdict(&rows[..1], 0), 0);
    }

    #[test]
    fn a_verdict_reads_as_a_word() {
        assert_eq!(verdict_word(Some(Verdict::Accept)), "accepted");
        assert_eq!(verdict_word(Some(Verdict::Reject)), "rejected");
        assert_eq!(verdict_word(Some(Verdict::Unsure)), "unsure");
        assert_eq!(verdict_word(None), "");
    }

    #[test]
    fn a_verdict_from_the_previous_review_says_so() {
        let mut rows = sample();
        rows[1].previous = Some(Verdict::Reject);
        rows[0].previous = Some(Verdict::Reject);
        assert_eq!(mine_text(&rows[1]), "rejected (from the previous review)");
        assert_eq!(mine_text(&rows[0]), "accepted");
        assert_eq!(mine_text(&rows[3]), "");
    }

    #[test]
    fn the_counter_says_how_many_i_reviewed() {
        assert_eq!(counter(&sample()), "2 of 4 reviewed");
    }

    #[test]
    fn the_decider_sees_the_most_accepted_first() {
        assert_eq!(by_accepts(&sample()), [1, 3, 0, 2]);
    }

    #[test]
    fn the_budget_attribute_comes_first_then_the_others_by_name() {
        let mut rows = sample();
        rows[0].event.attributes = json!({"zeta": 1, "mode": "burst"})
            .as_object()
            .unwrap()
            .clone();
        rows[1].event.attributes = json!({"alpha": 2}).as_object().unwrap().clone();
        assert_eq!(
            attribute_columns(&rows, Some("mode")),
            ["mode", "alpha", "zeta"]
        );
        assert_eq!(attribute_columns(&rows, None), ["alpha", "mode", "zeta"]);
        assert_eq!(attribute_columns(&rows[2..], Some("mode")), ["mode"]);
    }
}
