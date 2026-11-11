//! The open catalogue's events read a page at a time from the server while the project
//! downloads, so a big catalogue shows its first rows at once. The preview is read-only; once
//! the project has arrived the live table takes its place, at the same spot.

use std::collections::HashMap;

use cocat_core::model::{CatalogueSummary, EventRecord};
use serde::Deserialize;

use crate::period::Period;
use crate::rows::{Row, record_row};
use crate::sync::Loading;

/// Events asked for first: a screen's worth and more, quickly.
pub const FIRST_PAGE: usize = 1000;
/// Events asked for in each page after the first: the server's largest page.
pub const NEXT_PAGE: usize = 10_000;
/// Rows left below the view when the next page is asked for: a page takes about a second,
/// and half the first page is more than a visitor scrolls in that time.
const AHEAD: usize = FIRST_PAGE / 2;
/// What a column header says while the preview cannot sort.
pub const SORT_HINT: &str = "sorting once loaded";

/// One page of the catalogue's events route, in start order.
#[derive(Debug, Clone, Deserialize)]
pub struct EventsPage {
    pub events: Vec<EventRecord>,
    pub next: Option<String>,
    /// The names of the origins the events name, by id.
    #[serde(default)]
    pub origins: HashMap<String, String>,
}

/// The catalogues route's answer.
#[derive(Debug, Clone, Deserialize)]
pub struct Catalogues {
    pub catalogues: Vec<CatalogueSummary>,
}

/// The preview's rows so far, and the cursor of the page still to come.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewState {
    pub catalogue: CatalogueSummary,
    /// Every catalogue's (uuid, name), as the heading and the search line need them.
    pub names: Vec<(String, String)>,
    pub rows: Vec<Row>,
    pub next: Option<String>,
    /// A page did not come: the rest waits for the project.
    pub stalled: bool,
}

/// Where the visitor is in the table, carried from the preview to the live table.
#[derive(Debug, Clone, PartialEq)]
pub struct TableState {
    pub scroll_top: f64,
    /// The selected event's uuid: the live table reads it again from the project.
    pub selected: Option<String>,
    pub period: Option<Period>,
}

/// What the catalogue page shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Waiting,
    Preview,
    Live,
}

impl PreviewState {
    pub fn first(
        listed: &[CatalogueSummary],
        catalogue: CatalogueSummary,
        page: EventsPage,
    ) -> Self {
        Self {
            catalogue,
            names: listed
                .iter()
                .map(|listed| (listed.uuid.clone(), listed.name.clone()))
                .collect(),
            rows: page_rows(page.events, &page.origins),
            next: page.next,
            stalled: false,
        }
    }

    /// Adds the page that followed `cursor`; a page answered twice is added once.
    pub fn append(&mut self, cursor: &str, page: EventsPage) {
        if self.next.as_deref() != Some(cursor) {
            return;
        }
        self.rows.extend(page_rows(page.events, &page.origins));
        self.next = page.next;
    }

    /// The cursor of the page to ask for now: only as the view nears the end of the `shown`
    /// rows, so the pages do not hold up the project's download on the server.
    pub fn next_due(&self, view_end: usize, shown: usize) -> Option<&str> {
        let near = view_end + AHEAD >= shown;
        self.next.as_deref().filter(|_| near && !self.stalled)
    }

    /// Stops the reading after the page that followed `cursor` failed; an answer for a
    /// catalogue since left changes nothing.
    pub fn stall(&mut self, cursor: &str) {
        self.stalled |= self.next.as_deref() == Some(cursor);
    }

    pub fn header(&self) -> String {
        let (shown, total) = (self.rows.len(), self.catalogue.event_count);
        let rest = match self.stalled {
            false => "; the rest is loading…",
            true => ". Could not load more; the rest arrives with the project.",
        };
        format!("Showing {shown} of {total} events{rest}")
    }
}

/// An event without an author shows what made it, as the live table does.
fn page_rows(events: Vec<EventRecord>, origins: &HashMap<String, String>) -> Vec<Row> {
    events
        .into_iter()
        .map(|record| {
            let made_by = record
                .origin
                .as_ref()
                .and_then(|id| origins.get(id))
                .cloned();
            let row = record_row(record);
            Row {
                author: row.author.or(made_by),
                ..row
            }
        })
        .collect()
}

/// The preview shows while the project is still loading, once its first page is in: a
/// project that arrives first goes straight to the live table.
pub fn preview_due(loading: Option<&Loading>, first_page_arrived: bool) -> bool {
    loading.is_some() && first_page_arrived
}

pub fn stage(loading: Option<&Loading>, first_page_arrived: bool) -> Stage {
    match loading {
        None => Stage::Live,
        Some(_) if preview_due(loading, first_page_arrived) => Stage::Preview,
        Some(_) => Stage::Waiting,
    }
}

/// A route change keeps the preview only when it names the catalogue already shown: the
/// default route resolving to it, say. Any other drops the rows, the scroll and the selection.
pub fn keeps_preview(asked: Option<&str>, previewed: Option<&str>) -> bool {
    asked.is_some() && asked == previewed
}

pub fn switch_state(scroll_top: f64, selected: Option<&Row>, period: Option<Period>) -> TableState {
    TableState {
        scroll_top,
        selected: selected.and_then(|row| row.uuid.clone()),
        period,
    }
}

/// The title of a column header: the preview cannot sort, and says when it will.
pub fn sort_hint(sortable: bool) -> Option<&'static str> {
    (!sortable).then_some(SORT_HINT)
}

/// The catalogue the page will show: the one asked for, else the project's first one.
pub fn wanted<'a>(
    asked: Option<&str>,
    listed: &'a [CatalogueSummary],
) -> Option<&'a CatalogueSummary> {
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

    fn about(uuid: &str, count: usize) -> CatalogueSummary {
        serde_json::from_value(json!({
            "uuid": uuid, "name": format!("catalogue {uuid}"), "deleted": false, "tags": [],
            "columns": [], "attributes": {}, "copied_from": [], "event_count": count
        }))
        .unwrap()
    }

    fn page(events: &[(&str, &str)], next: Option<&str>) -> EventsPage {
        EventsPage {
            events: events
                .iter()
                .map(|(uuid, start)| record(uuid, start))
                .collect(),
            next: next.map(str::to_owned),
            origins: HashMap::new(),
        }
    }

    fn preview(count: usize, first: EventsPage) -> PreviewState {
        let catalogue = about("c", count);
        PreviewState::first(&[catalogue.clone(), about("d", 0)], catalogue, first)
    }

    fn uuids(state: &PreviewState) -> Vec<&str> {
        state
            .rows
            .iter()
            .filter_map(|row| row.uuid.as_deref())
            .collect()
    }

    const DOWNLOADING: Loading = Loading::Downloading {
        received: 0,
        total: None,
    };

    #[test]
    fn no_preview_when_the_project_arrives_first() {
        assert!(!preview_due(None, true), "the project is in: live table");
        assert!(!preview_due(None, false));
        assert!(!preview_due(Some(&DOWNLOADING), false), "loading note");
        assert!(preview_due(Some(&DOWNLOADING), true));
        assert!(preview_due(Some(&Loading::Opening), true));
    }

    #[test]
    fn the_page_goes_from_note_to_preview_to_live() {
        assert_eq!(stage(Some(&DOWNLOADING), false), Stage::Waiting);
        assert_eq!(stage(Some(&DOWNLOADING), true), Stage::Preview);
        assert_eq!(stage(Some(&Loading::Opening), true), Stage::Preview);
        assert_eq!(stage(None, true), Stage::Live, "preview to live");
        assert_eq!(stage(None, false), Stage::Live, "straight to live");
    }

    #[test]
    fn a_route_change_restarts_the_preview_unless_it_names_the_same_catalogue() {
        assert!(
            keeps_preview(Some("c"), Some("c")),
            "the default route resolved"
        );
        assert!(!keeps_preview(Some("d"), Some("c")));
        assert!(!keeps_preview(None, Some("c")));
        assert!(!keeps_preview(Some("c"), None), "nothing shown yet: start");
    }

    #[test]
    fn the_header_says_how_much_is_shown() {
        let mut state = preview(107_000, page(&[("a", "2020-01-01T00:00:00Z")], Some("c1")));
        assert_eq!(
            state.header(),
            "Showing 1 of 107000 events; the rest is loading…"
        );
        state.stall("c1");
        assert_eq!(
            state.header(),
            "Showing 1 of 107000 events. Could not load more; the rest arrives with the project."
        );
    }

    #[test]
    fn pages_append_in_start_order() {
        let first = page(
            &[("a", "2020-01-01T00:00:00Z"), ("b", "2020-01-02T00:00:00Z")],
            Some("c1"),
        );
        let mut state = preview(4, first);
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
        assert_eq!(state.names[1], ("d".to_owned(), "catalogue d".to_owned()));
    }

    #[test]
    fn a_failed_page_of_another_catalogue_does_not_stall_this_one() {
        let mut state = preview(20_000, page(&[("a", "2020-01-01T00:00:00Z")], Some("c1")));
        state.stall("cursor of the catalogue left");
        assert_eq!(state.next_due(4600, 5000), Some("c1"));
    }

    #[test]
    fn the_next_page_is_asked_only_near_the_end() {
        let mut state = preview(20_000, page(&[("a", "2020-01-01T00:00:00Z")], Some("c1")));
        assert_eq!(state.next_due(0, 5000), None, "far from the end");
        assert_eq!(state.next_due(4600, 5000), Some("c1"));
        assert_eq!(
            state.next_due(20, 1000),
            None,
            "the first screen asks nothing more"
        );
        assert_eq!(state.next_due(600, 1000), Some("c1"));
        state.stall("c1");
        assert_eq!(
            state.next_due(4600, 5000),
            None,
            "a failed page stops the reading"
        );
        state.stalled = false;
        state.next = None;
        assert_eq!(state.next_due(4600, 5000), None, "every page is in");
    }

    #[test]
    fn an_event_without_an_author_shows_its_origins_name() {
        let mut first = page(&[("a", "2020-01-01T00:00:00Z")], None);
        first.events[0].author = None;
        first.events[0].origin = Some("o1".into());
        first.origins.insert("o1".into(), "sw-finder".into());
        let state = preview(1, first);
        assert_eq!(state.rows[0].author.as_deref(), Some("sw-finder"));
    }

    #[test]
    fn the_selection_and_the_period_survive_the_switch() {
        let state = preview(1, page(&[("a", "2020-01-01T00:00:00Z")], None));
        let period = Some(Period::between(1, 2));
        let kept = switch_state(4000.0, state.rows.first(), period);
        assert_eq!(
            kept,
            TableState {
                scroll_top: 4000.0,
                selected: Some("a".into()),
                period,
            }
        );
        assert_eq!(switch_state(0.0, None, None).selected, None);
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
