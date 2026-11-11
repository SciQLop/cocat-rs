//! The open catalogue's events read a page at a time from the server while the project
//! downloads, so a big catalogue shows its first rows at once. The preview is read-only; once
//! the project has arrived the live table takes its place, at the same spot.

use cocat_core::model::EventRecord;
use serde::Deserialize;

use crate::rows::{Row, record_row};
use crate::sync::Loading;

/// Events asked for first: a screen's worth and more, quickly.
pub const FIRST_PAGE: usize = 1000;
/// Events asked for in each page after the first: the server's largest page.
pub const NEXT_PAGE: usize = 10_000;
/// What a column header says while the preview cannot sort.
pub const SORT_HINT: &str = "sorting once loaded";

/// One page of the catalogue's events route, in start order.
#[derive(Debug, Clone, Deserialize)]
pub struct EventsPage {
    pub events: Vec<EventRecord>,
    pub next: Option<String>,
}

/// What the preview needs of a catalogue's summary.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct About {
    pub uuid: String,
    pub name: String,
    #[serde(default)]
    pub columns: Vec<String>,
    pub event_count: usize,
}

/// The catalogues route's answer.
#[derive(Debug, Clone, Deserialize)]
pub struct Catalogues {
    pub catalogues: Vec<About>,
}

/// The preview's rows so far, and the cursor of the page still to come.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewState {
    pub about: About,
    pub rows: Vec<Row>,
    pub next: Option<String>,
}

/// Where the visitor is in the table, carried from the preview to the live table.
#[derive(Debug, Clone, PartialEq)]
pub struct TableState {
    pub scroll_top: f64,
    /// The selected event's uuid: the live table reads it again from the project.
    pub selected: Option<String>,
}

impl PreviewState {
    pub fn first(about: About, page: EventsPage) -> Self {
        Self {
            about,
            rows: page.events.into_iter().map(record_row).collect(),
            next: page.next,
        }
    }

    /// Adds the page that followed `cursor`; a page answered twice is added once.
    pub fn append(&mut self, cursor: &str, page: EventsPage) {
        if self.next.as_deref() != Some(cursor) {
            return;
        }
        self.rows.extend(page.events.into_iter().map(record_row));
        self.next = page.next;
    }

    pub fn header(&self) -> String {
        let (shown, total) = (self.rows.len(), self.about.event_count);
        format!("Showing {shown} of {total} events; the rest is loading…")
    }
}

/// The preview shows while the project is still loading, once its first page is in: a
/// project that arrives first goes straight to the live table.
pub fn preview_due(loading: Option<&Loading>, first_page_arrived: bool) -> bool {
    loading.is_some() && first_page_arrived
}

pub fn switch_state(scroll_top: f64, selected: Option<&Row>) -> TableState {
    TableState {
        scroll_top,
        selected: selected.and_then(|row| row.uuid.clone()),
    }
}

/// The title of a column header: the preview cannot sort, and says when it will.
pub fn sort_hint(sortable: bool) -> Option<&'static str> {
    (!sortable).then_some(SORT_HINT)
}

/// The catalogue the page will show: the one asked for, else the project's first one.
pub fn wanted<'a>(asked: Option<&str>, listed: &'a [About]) -> Option<&'a About> {
    match asked {
        Some(uuid) => listed.iter().find(|about| about.uuid == uuid),
        None => listed.first(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn record(uuid: &str, start: &str) -> EventRecord {
        serde_json::from_value(json!({
            "uuid": uuid, "start": start, "stop": start, "author": "alexis"
        }))
        .unwrap()
    }

    fn about(uuid: &str, count: usize) -> About {
        About {
            uuid: uuid.into(),
            name: format!("catalogue {uuid}"),
            columns: Vec::new(),
            event_count: count,
        }
    }

    fn page(events: &[(&str, &str)], next: Option<&str>) -> EventsPage {
        EventsPage {
            events: events
                .iter()
                .map(|(uuid, start)| record(uuid, start))
                .collect(),
            next: next.map(str::to_owned),
        }
    }

    fn uuids(state: &PreviewState) -> Vec<&str> {
        state
            .rows
            .iter()
            .filter_map(|row| row.uuid.as_deref())
            .collect()
    }

    #[test]
    fn no_preview_when_the_project_arrives_first() {
        assert!(!preview_due(None, true), "the project is in: live table");
        assert!(!preview_due(None, false));
        let downloading = Loading::Downloading {
            received: 0,
            total: None,
        };
        assert!(!preview_due(Some(&downloading), false), "loading note");
        assert!(preview_due(Some(&downloading), true));
        assert!(preview_due(Some(&Loading::Opening), true));
    }

    #[test]
    fn the_header_says_how_much_is_shown() {
        let first = page(&[("a", "2020-01-01T00:00:00Z")], Some("c1"));
        let state = PreviewState::first(about("c", 107_000), first);
        assert_eq!(
            state.header(),
            "Showing 1 of 107000 events; the rest is loading…"
        );
    }

    #[test]
    fn pages_append_in_start_order() {
        let first = page(
            &[("a", "2020-01-01T00:00:00Z"), ("b", "2020-01-02T00:00:00Z")],
            Some("c1"),
        );
        let mut state = PreviewState::first(about("c", 4), first);
        let second = page(
            &[("c", "2020-01-03T00:00:00Z"), ("d", "2020-01-04T00:00:00Z")],
            None,
        );
        state.append("c1", second.clone());
        assert_eq!(uuids(&state), ["a", "b", "c", "d"]);
        assert_eq!(state.next, None);
        state.append("c1", second);
        assert_eq!(
            uuids(&state),
            ["a", "b", "c", "d"],
            "a repeated page is ignored"
        );
        assert_eq!(state.rows[0].author.as_deref(), Some("alexis"));
    }

    #[test]
    fn the_selection_survives_the_switch() {
        let first = page(&[("a", "2020-01-01T00:00:00Z")], None);
        let state = PreviewState::first(about("c", 1), first);
        let kept = switch_state(4000.0, state.rows.first());
        assert_eq!(
            kept,
            TableState {
                scroll_top: 4000.0,
                selected: Some("a".into())
            }
        );
        assert_eq!(switch_state(0.0, None).selected, None);
    }

    #[test]
    fn sorting_is_off_in_the_preview() {
        assert_eq!(sort_hint(false), Some("sorting once loaded"));
        assert_eq!(sort_hint(true), None);
    }

    #[test]
    fn the_preview_shows_the_catalogue_asked_for_else_the_first() {
        let listed = [about("c1", 1), about("c2", 2)];
        assert_eq!(wanted(Some("c2"), &listed), Some(&listed[1]));
        assert_eq!(wanted(None, &listed), Some(&listed[0]));
        assert_eq!(wanted(Some("gone"), &listed), None);
        assert_eq!(wanted(None, &[]), None);
    }
}
