//! The room's history as the page shows it: the versions grouped by day, a version in one
//! line, and what a version changed as table rows.

use std::collections::HashSet;

use cocat_core::room_id::path_segment;
use cocat_core::time::Timestamp;
use serde::Deserialize;
use serde_json::{Map, Value};
use time::format_description::BorrowedFormatItem;
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};
use url::Url;
use yrs::updates::decoder::Decode;
use yrs::{Doc, Transact, Update};

use crate::format::{duration_text, time_text};
use crate::rows::{Row, rows};

/// A version as the server lists it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Version {
    pub id: u64,
    pub author_email: Option<String>,
    pub author_color: Option<String>,
    pub started_ms: Option<i64>,
    pub ended_ms: Option<i64>,
    pub via: Option<String>,
    pub auto_name: Option<String>,
    /// The name someone gave it, else the one its import or materialization gave it.
    pub name: Option<String>,
    pub events_added: u32,
    pub events_changed: u32,
    pub events_removed: u32,
    pub catalogues: Vec<String>,
    pub rules_changed: Vec<String>,
    /// The version a restore brought back, and when that version ended.
    pub restored_from: Option<u64>,
    pub restored_from_ms: Option<i64>,
    /// Someone saved it on purpose, with this note.
    #[serde(default)]
    pub committed: bool,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct History {
    pub versions: Vec<Version>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Change {
    Added,
    Changed,
    Removed,
}

/// What a version changed, as the server compares the room before and after it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Diff {
    pub events: Vec<EventChange>,
    pub catalogues: Vec<CatalogueChange>,
}

/// How many events were added, changed and removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct Tally {
    pub added: u32,
    pub changed: u32,
    pub removed: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CatalogueTally {
    pub name: String,
    #[serde(flatten)]
    pub tally: Tally,
}

/// The event changes of a version in all, and in each catalogue that holds some.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Counts {
    pub total: Tally,
    pub catalogues: Vec<CatalogueTally>,
}

/// One page of a version's changes, as the server answers a request with a `limit`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ChangesPage {
    pub counts: Counts,
    pub catalogues: Vec<CatalogueChange>,
    pub events: Vec<EventChange>,
    pub next: Option<String>,
}

/// The pages of a version's changes loaded so far, as table rows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Changes {
    pub counts: Counts,
    pub catalogues: Vec<CatalogueChange>,
    pub rows: Vec<DiffRow>,
    /// The cursor of the page still to load.
    pub next: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct EventChange {
    pub uuid: String,
    pub change: Change,
    pub fields: Vec<String>,
    pub before: Option<Map<String, Value>>,
    pub after: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CatalogueChange {
    pub uuid: String,
    pub name: String,
    pub change: Change,
    pub fields: Vec<String>,
}

/// The versions of one day, newest first.
#[derive(Debug, Clone, PartialEq)]
pub struct Day {
    pub label: String,
    pub versions: Vec<Version>,
}

/// One cell of the changes table.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Same(String),
    Changed {
        before: String,
        after: String,
    },
    /// A value of an event the version removed.
    Struck(String),
    /// What became of a removed event.
    Note(String),
}

/// One event of the changes table; `change` is `None` for an event the version left alone.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffRow {
    pub uuid: String,
    pub change: Option<Change>,
    pub start: i64,
    /// Start, duration, tags, confidence and status.
    pub cells: [Cell; 5],
    /// The other fields the version changed, which have no column.
    pub also: Vec<String>,
}

const COLUMNS: [&str; 5] = ["start", "stop", "tags", "confidence", "status"];
const DAY_MONTH: &[BorrowedFormatItem<'_>] =
    format_description!("[day padding:none] [month repr:long]");
const DAY_MONTH_YEAR: &[BorrowedFormatItem<'_>] =
    format_description!("[day padding:none] [month repr:long] [year]");
const WHEN: &[BorrowedFormatItem<'_>] =
    format_description!("[day padding:none] [month repr:short] [year], [hour]:[minute]");
const CLOCK: &[BorrowedFormatItem<'_>] = format_description!("[hour]:[minute]");

/// The versions grouped by the local day they ended on, newest first. `offset` gives the
/// visitor's UTC offset in minutes at an instant, so a day follows daylight saving.
pub fn by_day(versions: &[Version], now_ms: i64, offset: impl Fn(i64) -> i32) -> Vec<Day> {
    let today = local(now_ms, offset(now_ms)).map(|now| now.date());
    let mut days: Vec<Day> = Vec::new();
    for version in versions {
        let label = match (version.ended_ms, today) {
            (Some(ended), Some(today)) => day_label(ended, offset(ended), today),
            _ => "Before history".to_owned(),
        };
        match days.last_mut() {
            Some(day) if day.label == label => day.versions.push(version.clone()),
            _ => days.push(Day {
                label,
                versions: vec![version.clone()],
            }),
        }
    }
    days
}

/// The local time of day a version ended, "18:40".
pub fn clock(ms: i64, offset_minutes: i32) -> String {
    formatted(ms, offset_minutes, CLOCK)
}

/// A local date and time in full, "7 Nov 2026, 18:40".
pub fn when_text(ms: i64, offset_minutes: i32) -> String {
    formatted(ms, offset_minutes, WHEN)
}

/// Who made a version: the account's name before the "@", and what it went through.
pub fn author(version: &Version) -> String {
    let who = match (&version.author_email, version.started_ms) {
        (Some(email), _) => email.split('@').next().unwrap_or(email).to_owned(),
        (None, Some(_)) => "a deleted account".to_owned(),
        (None, None) => "before history".to_owned(),
    };
    match &version.via {
        Some(via) => format!("{who} via {via}"),
        None => who,
    }
}

/// A version in one line: "2 added, 1 changed, 1 removed", "Rule edited",
/// "Imported from tscat", "Restored the version of 7 Nov 2026, 18:40".
pub fn summary(version: &Version, offset: impl Fn(i64) -> i32) -> String {
    if version.restored_from.is_some() {
        return match version.restored_from_ms {
            Some(ended) => format!(
                "Restored the version of {}",
                when_text(ended, offset(ended))
            ),
            None => "Restored the version from before history".to_owned(),
        };
    }
    if let Some((kind, origin)) = version
        .auto_name
        .as_deref()
        .and_then(|name| name.split_once(": "))
    {
        match kind {
            "import" => return format!("Imported from {origin}"),
            "rule" => return format!("Frozen from {origin}"),
            _ => {}
        }
    }
    let counts = counts_text(
        version.events_added,
        version.events_changed,
        version.events_removed,
    );
    let rule = (!version.rules_changed.is_empty()).then(|| "Rule edited".to_owned());
    let parts: Vec<String> = rule.into_iter().chain(counts).collect();
    if !parts.is_empty() {
        return parts.join(", ");
    }
    match version.catalogues.len() {
        0 => "No change to show".to_owned(),
        1 => "Catalogue changed".to_owned(),
        count => format!("{count} catalogues changed"),
    }
}

/// "2 added, 1 changed, 1 removed", leaving out what is zero; `None` when all are.
pub fn counts_text(added: u32, changed: u32, removed: u32) -> Option<String> {
    let parts: Vec<String> = [(added, "added"), (changed, "changed"), (removed, "removed")]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, word)| format!("{count} {word}"))
        .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// "3 changed, 2 added, 1 removed", leaving out what is zero.
pub fn tally_text(tally: &Tally) -> String {
    let parts: Vec<String> = [
        (tally.changed, "changed"),
        (tally.added, "added"),
        (tally.removed, "removed"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, word)| format!("{count} {word}"))
    .collect();
    if parts.is_empty() {
        "no event changed".to_owned()
    } else {
        parts.join(", ")
    }
}

/// Events listed beyond the end of the view before the next page is asked for.
const NEAR_THE_END: usize = 60;

/// Whether the next page should be asked for: the view reaches the end of what is loaded, or
/// everything is wanted.
pub fn wants_next(view_end: usize, loaded: usize, next: Option<&str>, everything: bool) -> bool {
    next.is_some() && (everything || view_end + NEAR_THE_END >= loaded)
}

/// Events asked for in a page: few while the view scrolls through them, many when they all
/// load, since each request has a fixed cost.
pub fn page_size(everything: bool) -> usize {
    if everything { 10_000 } else { 200 }
}

impl Changes {
    pub fn first(page: ChangesPage) -> Self {
        Self {
            counts: page.counts,
            catalogues: page.catalogues,
            rows: diff_rows_of(&page.events),
            next: page.next,
        }
    }

    /// Adds the page that came after `cursor`; a page of any other cursor is a stale answer
    /// and is dropped.
    pub fn append(&mut self, cursor: &str, page: ChangesPage) {
        if self.next.as_deref() != Some(cursor) {
            return;
        }
        self.rows.extend(diff_rows_of(&page.events));
        self.next = page.next;
    }
}

/// How many events the diff added, changed and removed.
pub fn diff_counts(diff: &Diff) -> (u32, u32, u32) {
    let count = |change: Change| {
        let count = diff
            .events
            .iter()
            .filter(|event| event.change == change)
            .count();
        u32::try_from(count).unwrap_or(u32::MAX)
    };
    (
        count(Change::Added),
        count(Change::Changed),
        count(Change::Removed),
    )
}

/// The diff's events as table rows, in time order.
pub fn diff_rows(diff: &Diff) -> Vec<DiffRow> {
    diff_rows_of(&diff.events)
}

fn diff_rows_of(events: &[EventChange]) -> Vec<DiffRow> {
    let mut rows: Vec<DiffRow> = events.iter().filter_map(diff_row).collect();
    rows.sort_by_key(|row| row.start);
    rows
}

/// The events of `rows` (a catalogue as it was) the diff does not name, as unchanged rows.
/// Intervals a rule derived are not events, and are left out.
pub fn unchanged_rows(rows: &[Row], changed: &[DiffRow]) -> Vec<DiffRow> {
    let changed: HashSet<&str> = changed.iter().map(|row| row.uuid.as_str()).collect();
    rows.iter()
        .filter_map(|row| {
            let uuid = row.uuid.as_ref()?;
            (!changed.contains(uuid.as_str())).then(|| DiffRow {
                uuid: uuid.clone(),
                change: None,
                start: row.start,
                cells: [
                    time_text(row.start),
                    duration_text(row.stop - row.start),
                    row.tags.join(", "),
                    row.confidence.map(confidence_text).unwrap_or_default(),
                    row.status.name().to_owned(),
                ]
                .map(Cell::Same),
                also: Vec::new(),
            })
        })
        .collect()
}

/// The rows of a catalogue in a past state of the room, given as one Yjs update.
pub fn past_rows(update: &[u8], catalogue: &str) -> Result<Vec<Row>, String> {
    let update = Update::decode_v1(update).map_err(|error| error.to_string())?;
    let doc = Doc::new();
    doc.transact_mut()
        .apply_update(update)
        .map_err(|error| error.to_string())?;
    rows(&doc.transact(), catalogue).map_err(|error| error.to_string())
}

/// A catalogue change in words: "“Strong waves” created", "“Strong waves”: rule changed".
pub fn catalogue_line(change: &CatalogueChange) -> String {
    let name = format!("“{}”", change.name);
    match change.change {
        Change::Added => format!("{name} created"),
        Change::Removed if change.fields.is_empty() => format!("{name} deleted for good"),
        Change::Removed => format!("{name} moved to the trash"),
        Change::Changed => {
            let fields: Vec<&str> = change
                .fields
                .iter()
                .map(|field| field_word(field))
                .collect();
            format!("{name}: {} changed", fields.join(", "))
        }
    }
}

/// What "Save version" sends: a blank name or note is none.
pub fn save_body(name: &str, note: &str) -> Value {
    let given = |text: &str| {
        Some(text.trim())
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    serde_json::json!({ "name": given(name), "message": given(note) })
}

/// The name "Make a copy from this version" suggests.
pub fn copy_suggestion(project: &str) -> String {
    format!("{project} (copy)")
}

/// The catalogue browser of another project, relative to the app's `<base>`.
pub fn browse_href(room_id: &str) -> String {
    format!("../rooms/{}/browse", path_segment(room_id))
}

/// Reloads of the version list, one request at a time: a reload asked for while one runs
/// waits for it, and several such asks make one more reload.
#[derive(Debug, Default)]
pub struct Reloads {
    running: bool,
    again: bool,
}

impl Reloads {
    /// A reload is wanted; true when it should start now.
    pub fn ask(&mut self) -> bool {
        self.again = self.running;
        !std::mem::replace(&mut self.running, true)
    }

    /// The running reload ended; true when another should start now.
    pub fn done(&mut self) -> bool {
        self.running = std::mem::take(&mut self.again);
        self.running
    }
}

/// The server's reason for refusing a request, from its `{"detail": "…"}` body.
pub fn refusal_text(status: u16, body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|body| body.get("detail")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("the server answered {status}"))
}

/// A history route of the API: `<api>/rooms/<room>/history/<path…>?<query>`, each part
/// escaped as one segment.
pub fn endpoint(api: &str, room: &str, path: &[&str], query: &[(&str, &str)]) -> Option<String> {
    let mut url = Url::parse(api).ok()?;
    url.path_segments_mut()
        .ok()?
        .pop_if_empty()
        .extend(["rooms", room, "history"])
        .extend(path);
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }
    Some(url.into())
}

fn diff_row(event: &EventChange) -> Option<DiffRow> {
    let shown = event.after.as_ref().or(event.before.as_ref())?;
    let start = nanos(shown.get("start"))?;
    let texts = |record: Option<&Map<String, Value>>| record.map(column_texts);
    let (before, after) = (texts(event.before.as_ref()), texts(event.after.as_ref()));
    let cells = match (event.change, before, after) {
        (Change::Removed, Some(before), _) => {
            let [start, duration, tags, confidence, _] = before;
            let note = match event.after {
                Some(_) => "moved to trash",
                None => "deleted for good",
            };
            [
                Cell::Struck(start),
                Cell::Struck(duration),
                Cell::Struck(tags),
                Cell::Struck(confidence),
                Cell::Note(note.to_owned()),
            ]
        }
        (Change::Changed, Some(before), Some(after)) => {
            let mut pairs = before.into_iter().zip(after);
            std::array::from_fn(|_| match pairs.next() {
                Some((before, after)) if before != after => Cell::Changed { before, after },
                Some((_, after)) => Cell::Same(after),
                None => Cell::Same(String::new()),
            })
        }
        (_, _, Some(after)) => after.map(Cell::Same),
        (_, Some(before), None) => before.map(Cell::Same),
        (_, None, None) => return None,
    };
    let also = match event.change {
        Change::Changed => event
            .fields
            .iter()
            .filter(|field| !COLUMNS.contains(&field.as_str()))
            .map(|field| field_word(field).to_owned())
            .collect(),
        Change::Added | Change::Removed => Vec::new(),
    };
    Some(DiffRow {
        uuid: event.uuid.clone(),
        change: Some(event.change),
        start,
        cells,
        also,
    })
}

/// Start, duration, tags, confidence and status of an event record, as the table reads them.
fn column_texts(record: &Map<String, Value>) -> [String; 5] {
    let start = nanos(record.get("start"));
    let stop = nanos(record.get("stop"));
    let tags: Vec<&str> = record
        .get("tags")
        .and_then(Value::as_array)
        .map(|tags| tags.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    [
        start.map(time_text).unwrap_or_default(),
        start
            .zip(stop)
            .map(|(start, stop)| duration_text(stop - start))
            .unwrap_or_default(),
        tags.join(", "),
        record
            .get("confidence")
            .and_then(Value::as_f64)
            .map(confidence_text)
            .unwrap_or_default(),
        record
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    ]
}

fn confidence_text(confidence: f64) -> String {
    format!("{confidence:.2}")
}

fn nanos(text: Option<&Value>) -> Option<i64> {
    Timestamp::parse(text?.as_str()?)?.unix_nanos()
}

/// A field as a person names it.
fn field_word(field: &str) -> &str {
    match field {
        "events" => "members",
        "start_err" => "start error",
        "stop_err" => "stop error",
        "source_id" => "source",
        other => other,
    }
}

fn day_label(ms: i64, offset_minutes: i32, today: time::Date) -> String {
    let Some(day) = local(ms, offset_minutes) else {
        return "Before history".to_owned();
    };
    let format = match (today - day.date()).whole_days() {
        0 => return "Today".to_owned(),
        1 => return "Yesterday".to_owned(),
        _ if day.year() == today.year() => DAY_MONTH,
        _ => DAY_MONTH_YEAR,
    };
    day.format(format).unwrap_or_default()
}

fn local(ms: i64, offset_minutes: i32) -> Option<OffsetDateTime> {
    let offset = UtcOffset::from_whole_seconds(offset_minutes.checked_mul(60)?).ok()?;
    let utc = OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok()?;
    Some(utc.to_offset(offset))
}

fn formatted(ms: i64, offset_minutes: i32, format: &[BorrowedFormatItem<'_>]) -> String {
    local(ms, offset_minutes)
        .and_then(|time| time.format(format).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::fixture::room;
    use yrs::ReadTxn;

    #[test]
    fn a_copy_is_suggested_under_the_project_name() {
        assert_eq!(copy_suggestion("Storms 2024"), "Storms 2024 (copy)");
    }

    #[test]
    fn the_copy_opens_in_its_own_browser() {
        assert_eq!(browse_href("4f2a"), "../rooms/4f2a/browse");
        assert_eq!(
            browse_href("Bepi MSA?#%"),
            "../rooms/Bepi%20MSA%3F%23%25/browse"
        );
    }

    /// 2026-11-08 12:00 UTC.
    const NOW: i64 = 1_794_139_200_000;
    const HOUR: i64 = 3_600_000;
    /// Paris in winter.
    const PARIS: i32 = 60;

    fn version(id: u64, ended_ms: Option<i64>) -> Version {
        Version {
            id,
            author_email: Some("b.lavraud@example.org".to_owned()),
            author_color: Some("#7c4dcc".to_owned()),
            started_ms: ended_ms,
            ended_ms,
            via: None,
            auto_name: None,
            name: None,
            events_added: 0,
            events_changed: 0,
            events_removed: 0,
            catalogues: Vec::new(),
            rules_changed: Vec::new(),
            restored_from: None,
            restored_from_ms: None,
            committed: false,
            message: None,
        }
    }

    #[test]
    fn a_saved_version_reads_with_its_note() {
        let listed = json!({
            "id": 3, "author_email": null, "author_color": null, "started_ms": 1,
            "ended_ms": 2, "via": null, "auto_name": null, "name": "paper v1",
            "events_added": 1, "events_changed": 0, "events_removed": 0,
            "catalogues": [], "rules_changed": [], "restored_from": null,
            "restored_from_ms": null, "committed": true, "message": "as submitted"
        });

        let version: Version = serde_json::from_value(listed).unwrap();

        assert!(version.committed);
        assert_eq!(version.message.as_deref(), Some("as submitted"));
    }

    #[test]
    fn saving_leaves_a_blank_name_or_note_out() {
        assert_eq!(
            save_body("  ", " looked at it \n"),
            json!({ "name": null, "message": "looked at it" })
        );
        assert_eq!(
            save_body("v1", ""),
            json!({ "name": "v1", "message": null })
        );
    }

    #[test]
    fn a_reload_starts_when_none_runs() {
        let mut reloads = Reloads::default();

        assert!(reloads.ask());
        assert!(!reloads.done());
        assert!(reloads.ask());
    }

    #[test]
    fn asks_during_a_reload_make_one_more() {
        let mut reloads = Reloads::default();
        assert!(reloads.ask());

        assert!(!reloads.ask());
        assert!(!reloads.ask());

        assert!(reloads.done());
        assert!(!reloads.done());
        assert!(reloads.ask());
    }

    fn labels(days: &[Day]) -> Vec<(&str, Vec<u64>)> {
        days.iter()
            .map(|day| {
                let ids = day.versions.iter().map(|version| version.id).collect();
                (day.label.as_str(), ids)
            })
            .collect()
    }

    #[test]
    fn versions_group_by_the_local_day_they_ended() {
        let versions = [
            version(6, Some(NOW - HOUR)),
            // 23:30 UTC the day before, 00:30 in Paris: today.
            version(5, Some(NOW - 12 * HOUR - HOUR / 2)),
            version(4, Some(NOW - 13 * HOUR - HOUR / 2)),
            version(3, Some(NOW - 2 * 24 * HOUR)),
            version(2, Some(NOW - 370 * 24 * HOUR)),
            version(1, None),
        ];

        let days = by_day(&versions, NOW, |_| PARIS);

        assert_eq!(
            labels(&days),
            [
                ("Today", vec![6, 5]),
                ("Yesterday", vec![4]),
                ("6 November", vec![3]),
                ("3 November 2025", vec![2]),
                ("Before history", vec![1]),
            ]
        );
    }

    #[test]
    fn a_day_follows_the_offset_at_its_own_instant() {
        let versions = [version(1, Some(NOW - 12 * HOUR - HOUR / 2))];
        let summer_then = |ms: i64| if ms < NOW - HOUR { 0 } else { PARIS };

        assert_eq!(
            labels(&by_day(&versions, NOW, summer_then))[0].0,
            "Yesterday"
        );
    }

    #[test]
    fn times_read_in_local_time() {
        assert_eq!(clock(NOW, PARIS), "13:00");
        assert_eq!(when_text(NOW, PARIS), "8 Nov 2026, 13:00");
    }

    #[test]
    fn the_author_is_the_name_before_the_at_and_what_it_went_through() {
        let mut detector = version(1, Some(NOW));
        detector.via = Some("sw-finder 1.2".to_owned());
        assert_eq!(author(&detector), "b.lavraud via sw-finder 1.2");
        let mut gone = version(2, Some(NOW));
        gone.author_email = None;
        assert_eq!(author(&gone), "a deleted account");
        let mut old = version(3, None);
        old.author_email = None;
        assert_eq!(author(&old), "before history");
    }

    #[test]
    fn a_summary_counts_what_changed() {
        let mut edits = version(1, Some(NOW));
        (
            edits.events_added,
            edits.events_changed,
            edits.events_removed,
        ) = (2, 1, 1);
        assert_eq!(summary(&edits, |_| PARIS), "2 added, 1 changed, 1 removed");
        edits.events_removed = 0;
        assert_eq!(summary(&edits, |_| PARIS), "2 added, 1 changed");
    }

    #[test]
    fn a_summary_says_a_rule_was_edited() {
        let mut rule = version(1, Some(NOW));
        rule.rules_changed = vec!["c".to_owned()];
        rule.catalogues = vec!["c".to_owned()];
        assert_eq!(summary(&rule, |_| PARIS), "Rule edited");
        rule.events_changed = 3;
        assert_eq!(summary(&rule, |_| PARIS), "Rule edited, 3 changed");
    }

    #[test]
    fn an_import_or_a_frozen_catalogue_says_where_from() {
        let mut import = version(1, Some(NOW));
        import.events_added = 120;
        import.auto_name = Some("import: tscat".to_owned());
        assert_eq!(summary(&import, |_| PARIS), "Imported from tscat");
        import.auto_name = Some("rule: Strong waves".to_owned());
        assert_eq!(summary(&import, |_| PARIS), "Frozen from Strong waves");
    }

    #[test]
    fn a_restore_names_the_version_it_brought_back_not_its_counts() {
        let mut restore = version(5, Some(NOW));
        restore.events_changed = 4;
        restore.restored_from = Some(2);
        restore.restored_from_ms = Some(NOW - HOUR);
        assert_eq!(
            summary(&restore, |_| PARIS),
            "Restored the version of 8 Nov 2026, 12:00"
        );
        restore.restored_from_ms = None;
        assert_eq!(
            summary(&restore, |_| PARIS),
            "Restored the version from before history"
        );
    }

    #[test]
    fn a_version_that_only_touched_catalogues_says_so() {
        let mut members = version(1, Some(NOW));
        members.catalogues = vec!["c".to_owned()];
        assert_eq!(summary(&members, |_| PARIS), "Catalogue changed");
        members.catalogues.push("d".to_owned());
        assert_eq!(summary(&members, |_| PARIS), "2 catalogues changed");
        assert_eq!(
            summary(&version(2, Some(NOW)), |_| PARIS),
            "No change to show"
        );
    }

    fn record(start: &str, confidence: f64, status: &str, tags: &[&str]) -> Map<String, Value> {
        let value = json!({
            "uuid": "e", "start": start, "stop": "2019-11-14 03:12:41+00:00",
            "confidence": confidence, "status": status, "tags": tags, "comment": null,
        });
        value.as_object().cloned().unwrap()
    }

    fn change(
        uuid: &str,
        change: Change,
        fields: &[&str],
        before: Option<Map<String, Value>>,
        after: Option<Map<String, Value>>,
    ) -> EventChange {
        EventChange {
            uuid: uuid.to_owned(),
            change,
            fields: fields.iter().map(|field| (*field).to_owned()).collect(),
            before,
            after,
        }
    }

    const START: &str = "2019-11-14 03:12:08+00:00";

    #[test]
    fn a_changed_event_shows_old_and_new_where_they_differ() {
        let diff = Diff {
            events: vec![change(
                "e",
                Change::Changed,
                &["comment", "confidence", "status"],
                Some(record(START, 0.6, "preliminary", &["sw"])),
                Some(record(START, 0.9, "reviewed", &["sw"])),
            )],
            catalogues: Vec::new(),
        };

        let rows = diff_rows(&diff);

        assert_eq!(
            rows[0].cells,
            [
                Cell::Same("2019-11-14 03:12:08".to_owned()),
                Cell::Same("33 s".to_owned()),
                Cell::Same("sw".to_owned()),
                Cell::Changed {
                    before: "0.60".to_owned(),
                    after: "0.90".to_owned()
                },
                Cell::Changed {
                    before: "preliminary".to_owned(),
                    after: "reviewed".to_owned()
                },
            ]
        );
        assert_eq!(rows[0].also, ["comment"]);
        assert_eq!(rows[0].change, Some(Change::Changed));
    }

    #[test]
    fn a_removed_event_is_struck_and_says_where_it_went() {
        let before = record(START, 0.41, "final", &["sw"]);
        let trashed = record(START, 0.41, "deleted", &["sw"]);
        let diff = Diff {
            events: vec![
                change(
                    "t",
                    Change::Removed,
                    &["status"],
                    Some(before.clone()),
                    Some(trashed),
                ),
                change("p", Change::Removed, &[], Some(before), None),
            ],
            catalogues: Vec::new(),
        };

        let rows = diff_rows(&diff);

        assert_eq!(
            rows[0].cells[0],
            Cell::Struck("2019-11-14 03:12:08".to_owned())
        );
        assert_eq!(rows[0].cells[4], Cell::Note("moved to trash".to_owned()));
        assert_eq!(rows[1].cells[4], Cell::Note("deleted for good".to_owned()));
        assert!(rows[0].also.is_empty());
    }

    #[test]
    fn diff_rows_are_in_time_order_and_added_ones_show_their_values() {
        let diff = Diff {
            events: vec![
                change(
                    "late",
                    Change::Added,
                    &[],
                    None,
                    Some(record(
                        "2019-11-14 03:12:30+00:00",
                        0.95,
                        "final",
                        &["sw", "burst"],
                    )),
                ),
                change(
                    "early",
                    Change::Added,
                    &[],
                    None,
                    Some(record(START, 0.93, "final", &[])),
                ),
            ],
            catalogues: Vec::new(),
        };

        let rows = diff_rows(&diff);

        assert_eq!(
            rows.iter().map(|row| row.uuid.as_str()).collect::<Vec<_>>(),
            ["early", "late"]
        );
        assert_eq!(rows[1].cells[2], Cell::Same("sw, burst".to_owned()));
        assert_eq!(rows[1].cells[3], Cell::Same("0.95".to_owned()));
        assert_eq!(diff_counts(&diff), (2, 0, 0));
    }

    #[test]
    fn unchanged_rows_are_the_events_the_diff_leaves_out() {
        let doc = room();
        let members = rows(&doc.transact(), "c-static").unwrap();
        let first = members[0].uuid.clone().unwrap();
        let changed = diff_rows(&Diff {
            events: vec![change(
                &first,
                Change::Changed,
                &["status"],
                Some(record(START, 0.6, "final", &[])),
                Some(record(START, 0.9, "final", &[])),
            )],
            catalogues: Vec::new(),
        });

        let unchanged = unchanged_rows(&members, &changed);

        assert_eq!(unchanged.len(), members.len() - 1);
        assert!(
            unchanged
                .iter()
                .all(|row| row.uuid != first && row.change.is_none())
        );
        assert!(
            matches!(&unchanged[0].cells[0], Cell::Same(text) if text.starts_with("2019-11-0"))
        );
    }

    #[test]
    fn a_past_state_reads_as_the_rows_of_its_catalogue() {
        let doc = room();
        let update = doc
            .transact()
            .encode_state_as_update_v1(&yrs::StateVector::default());

        let past = past_rows(&update, "c-static").unwrap();

        assert_eq!(past, rows(&doc.transact(), "c-static").unwrap());
        assert!(past_rows(b"not an update", "c-static").is_err());
    }

    #[test]
    fn catalogue_changes_read_in_words() {
        let line = |change: Change, fields: &[&str]| {
            catalogue_line(&CatalogueChange {
                uuid: "c".to_owned(),
                name: "Strong waves".to_owned(),
                change,
                fields: fields.iter().map(|field| (*field).to_owned()).collect(),
            })
        };
        assert_eq!(line(Change::Added, &[]), "“Strong waves” created");
        assert_eq!(
            line(Change::Changed, &["events", "rule"]),
            "“Strong waves”: members, rule changed"
        );
        assert_eq!(
            line(Change::Removed, &["deleted"]),
            "“Strong waves” moved to the trash"
        );
        assert_eq!(
            line(Change::Removed, &[]),
            "“Strong waves” deleted for good"
        );
    }

    #[test]
    fn a_refusal_reads_the_servers_detail() {
        assert_eq!(
            refusal_text(403, r#"{"detail": "Project lab requires the editor role"}"#),
            "Project lab requires the editor role"
        );
        assert_eq!(refusal_text(502, "<html>"), "the server answered 502");
    }

    #[test]
    fn endpoints_escape_each_segment() {
        let api = "https://host/cocat-dev/api";
        assert_eq!(
            endpoint(api, "a/b", &[], &[("catalogue", "c 1")]).as_deref(),
            Some("https://host/cocat-dev/api/rooms/a%2Fb/history?catalogue=c+1")
        );
        assert_eq!(
            endpoint(api, "lab", &["12", "restore"], &[]).as_deref(),
            Some("https://host/cocat-dev/api/rooms/lab/history/12/restore")
        );
        assert_eq!(
            endpoint("https://host/api/", "lab", &[], &[]).as_deref(),
            Some("https://host/api/rooms/lab/history")
        );
    }

    fn page_of(uuids: &[&str], next: Option<&str>) -> ChangesPage {
        let added = |uuid: &&str| {
            change(
                uuid,
                Change::Added,
                &[],
                None,
                Some(record(START, 0.9, "final", &[])),
            )
        };
        ChangesPage {
            events: uuids.iter().map(added).collect(),
            next: next.map(str::to_owned),
            ..ChangesPage::default()
        }
    }

    fn uuids_of(changes: &Changes) -> Vec<&str> {
        changes.rows.iter().map(|row| row.uuid.as_str()).collect()
    }

    #[test]
    fn the_summary_reads_the_counts() {
        let page: ChangesPage = serde_json::from_value(serde_json::json!({
            "counts": {
                "total": {"added": 2, "changed": 3, "removed": 1},
                "catalogues": [{"catalogue": "c", "name": "Waves", "added": 0, "changed": 4, "removed": 0}]
            },
            "catalogues": [],
            "events": [],
            "next": null
        }))
        .unwrap();

        assert_eq!(
            tally_text(&page.counts.total),
            "3 changed, 2 added, 1 removed"
        );
        assert_eq!(page.counts.catalogues[0].name, "Waves");
        assert_eq!(tally_text(&page.counts.catalogues[0].tally), "4 changed");
        assert_eq!(tally_text(&Tally::default()), "no event changed");
    }

    #[test]
    fn the_next_page_is_asked_near_the_end() {
        assert!(wants_next(150, 200, Some("c"), false));
        assert!(!wants_next(20, 200, Some("c"), false));
        assert!(!wants_next(200, 200, None, false));
        assert!(wants_next(0, 200, Some("c"), true));
    }

    #[test]
    fn loading_everything_asks_for_bigger_pages() {
        assert_eq!(page_size(false), 200);
        assert!(page_size(true) >= 10_000);
    }

    #[test]
    fn pages_are_appended_in_order() {
        let mut changes = Changes::first(page_of(&["a", "b"], Some("one")));

        changes.append("one", page_of(&["c"], Some("two")));
        changes.append("two", page_of(&["d"], None));

        assert_eq!(uuids_of(&changes), ["a", "b", "c", "d"]);
        assert_eq!(changes.next, None);
    }

    #[test]
    fn a_page_that_does_not_follow_the_loaded_ones_is_dropped() {
        let mut changes = Changes::first(page_of(&["a"], Some("one")));

        changes.append("elsewhere", page_of(&["x"], None));

        assert_eq!(uuids_of(&changes), ["a"]);
        assert_eq!(changes.next.as_deref(), Some("one"));
    }
}
