//! The history: the versions of what was on screen, what one of them changed, and, for
//! editors, naming and restoring it. Also the event panel's latest changes.

use std::ops::Range;
use std::time::Duration;

use leptos::html::{Div, Input, Textarea};
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsValue;

use crate::api;
use crate::history::{
    Cell, Change, Changes, ChangesPage, Day, DiffRow, Reloads, Version, author, browse_href,
    by_day, catalogue_line, clock, copy_suggestion, counts_text, past_rows, summary, tally_text,
    unchanged_rows, wants_next, when_text,
};
use crate::layout::window as visible_rows;
use crate::route::Route;
use crate::rows::Row;
use crate::state::Room;
use crate::views::controls::{Error, Hint, confirm, error_line, go_to};
use crate::views::projects::Projects;

/// Every changes row's height in pixels; the stylesheet sets the same.
const ROW: f64 = 40.0;
/// The event panel's latest changes.
const LAST_CHANGES: usize = 5;

/// How long the list waits after the project's last change before it reloads.
const LIVE_DELAY: Duration = Duration::from_secs(2);

type Versions = LocalResource<Result<Vec<Version>, String>>;
type Loaded = RwSignal<Option<Result<Vec<Version>, String>>>;

/// `scope` (the catalogue, else the room) and `picked` (the version) belong to the page
/// around this view and keep their values when it leaves the history, so nothing here
/// reloads while it is taken down.
#[component]
pub fn HistoryPage(scope: Memo<Option<String>>, picked: Memo<Option<u64>>) -> impl IntoView {
    let room = expect_context::<Room>();
    let versions: Loaded = RwSignal::new(None);
    let reload = reloader(&room, scope, versions);
    reload_when_the_project_changes(&room, reload);
    Effect::new(move |_| {
        scope.track();
        versions.set(None);
        reload.run(());
    });
    let version = Memo::new(move |_| {
        let id = picked.get()?;
        versions.with(|versions| {
            let versions = versions.as_ref()?.as_ref().ok()?;
            versions.iter().find(|version| version.id == id).cloned()
        })
    });
    view! {
        {move || picked.get().map(|id| view! { <Banner id version scope reload/> })}
        <div class="history">
            <main class="content history-main">
                {move || match picked.get() {
                    Some(id) => view! { <ChangesPanel id scope/> }.into_any(),
                    None => view! {
                        <div class="page-head"><h1>"History"</h1></div>
                        <p class="quiet">"Pick a version to see what it changed."</p>
                    }.into_any(),
                }}
            </main>
            <VersionList versions scope picked reload/>
        </div>
    }
}

/// Loads the versions of `scope` into `versions`, one request at a time. An answer for a
/// scope left meanwhile is dropped: the reload that follows it brings the right one.
fn reloader(room: &Room, scope: Memo<Option<String>>, versions: Loaded) -> Callback<()> {
    let reloads = StoredValue::new(Reloads::default());
    let config = room.config.clone();
    Callback::new(move |()| {
        if reloads.try_update_value(Reloads::ask) != Some(true) {
            return;
        }
        let config = config.clone();
        spawn_local(async move {
            loop {
                let Some(wanted) = scope.try_get_untracked() else {
                    return;
                };
                let answer = api::history(&config, wanted.as_deref(), None)
                    .await
                    .map_err(|error| error.to_string());
                if scope.try_get_untracked().as_ref() == Some(&wanted) {
                    versions.try_set(Some(answer));
                }
                if reloads.try_update_value(Reloads::done) != Some(true) {
                    return;
                }
            }
        });
    })
}

/// Anyone's edit, arriving or made here, reloads the list once the project is quiet.
fn reload_when_the_project_changes(room: &Room, reload: Callback<()>) {
    let timer = StoredValue::new(None::<TimeoutHandle>);
    let stop = move || {
        timer.try_update_value(|timer| timer.take().map(|handle| handle.clear()));
    };
    let room = room.clone();
    Effect::new(move |seen: Option<()>| {
        room.track();
        if seen.is_some() {
            stop();
            let handle = set_timeout_with_handle(move || reload.run(()), LIVE_DELAY).ok();
            timer.try_set_value(handle);
        }
    });
    on_cleanup(stop);
}

/// What is on screen, and the read-only notice with "Back to now", making a copy and, for
/// editors, naming and restoring.
#[component]
fn Banner(
    id: u64,
    version: Memo<Option<Version>>,
    scope: Memo<Option<String>>,
    reload: Callback<()>,
) -> impl IntoView {
    let room = expect_context::<Room>();
    let projects = expect_context::<Projects>();
    let page = page();
    let reader = room.clone();
    let subject = Memo::new(move |_| match scope.get() {
        Some(uuid) => reader
            .read(|txn| cocat_core::model::catalogue(txn, &uuid))
            .map_or_else(|| "this catalogue".to_owned(), |catalogue| catalogue.name),
        None => projects.open.with(|config| config.room_name.clone()),
    });
    let when = move || {
        version.with(|version| {
            let ended = version.as_ref()?.ended_ms?;
            Some(when_text(ended, offset(ended)))
        })
    };
    let after = move || {
        version.with(|version| match version {
            Some(version) => format!("after {}'s edits", author(version)),
            None => format!("after version {id}"),
        })
    };
    let message: Error = RwSignal::new(None);
    let naming = RwSignal::new(false);
    let copying = RwSignal::new(false);
    let live = move || history(scope.get(), None).live().href(&page);
    let restore = {
        let room = room.clone();
        move |_| {
            let question = format!(
                "Restore {} as it was {}? This adds one change, which the history keeps: \
                 restoring the version before it undoes it.",
                subject.get_untracked(),
                when().map_or_else(after, |when| format!("on {when}")),
            );
            if !confirm(&question) {
                return;
            }
            let config = room.config.clone();
            let catalogue = scope.get_untracked();
            spawn_local(async move {
                match api::restore(&config, id, catalogue.as_deref()).await {
                    Ok(true) => go_to(&history(catalogue, None).live().hash()),
                    Ok(false) => message.set(Some(
                        "Nothing to restore: it already equals this version.".to_owned(),
                    )),
                    Err(error) => message.set(Some(error.to_string())),
                }
            });
        }
    };
    let editor = room.can_write();
    view! {
        <div class="history-banner">
            <HistoryIcon/>
            <div class="banner-text">
                <p>
                    "You are looking at " <strong>{move || subject.get()}</strong>
                    {move || when().map(|when| view! { " as it was on " <strong>{when}</strong> })}
                    ", " {after} ". It is read-only."
                </p>
                {move || note(version).map(|note| view! { <p class="version-note">{note}</p> })}
            </div>
            <div class="banner-buttons">
                <a class="button" href=live>"Back to now"</a>
                <button type="button" class="button" aria-expanded=move || copying.get().to_string()
                    on:click=move |_| copying.update(|open| *open = !*open)>"Make a copy from this version"</button>
                {editor.then(|| view! {
                    <button type="button" class="button" aria-expanded=move || naming.get().to_string()
                        on:click=move |_| naming.update(|open| *open = !*open)>"Name this version"</button>
                    <button type="button" class="button primary" on:click=restore>"Restore this version"</button>
                })}
            </div>
        </div>
        {error_line(message)}
        <Show when=move || naming.get()>
            <NameVersion id version open=naming reload/>
        </Show>
        <Show when=move || copying.get()>
            <CopyVersion id open=copying/>
        </Show>
    }
}

/// Makes a new project from the version, then links to it.
#[component]
fn CopyVersion(id: u64, open: RwSignal<bool>) -> impl IntoView {
    let config = expect_context::<Room>().config.clone();
    let input = NodeRef::<Input>::new();
    let error: Error = RwSignal::new(None);
    let made = RwSignal::new(None::<String>);
    Effect::new(move |_| {
        if let Some(input) = input.get() {
            let _ = input.focus();
            input.select();
        }
    });
    let name = expect_context::<Projects>()
        .open
        .with_untracked(|open| open.room_name.clone());
    let suggestion = copy_suggestion(&name);
    let send = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let name = input.get().map(|input| input.value()).unwrap_or_default();
        let config = config.clone();
        spawn_local(async move {
            match api::copy_version(&config, id, &name).await {
                Ok(room) => made.set(Some(room)),
                Err(failure) => error.set(Some(failure.to_string())),
            }
        });
    };
    view! {
        <Show when=move || made.with(Option::is_none) fallback=move || view! {
            <div class="name-form version-name copy-done" role="status">
                <p>"The copy is ready. It is a new project of yours; this one stays as it is."</p>
                <div class="row-buttons">
                    <a class="button primary" href=move || made.get().map(|room| browse_href(&room))>"Open the copy"</a>
                    <button type="button" class="button" on:click=move |_| open.set(false)>"Close"</button>
                </div>
            </div>
        }>
            <form class="name-form version-name" on:submit=send.clone()>
                <Hint anchor="history" topic="copying a version"
                    text="The copy is a new project: the whole project as it was at this version. Try things there; the original does not change."/>
                <label>
                    <span class="field-label">"Name of the copy"</span>
                    <input type="text" maxlength="200" node_ref=input value=suggestion.clone()/>
                </label>
                {error_line(error)}
                <div class="row-buttons">
                    <button type="submit" class="button primary">"Make the copy"</button>
                    <button type="button" class="button" on:click=move |_| open.set(false)>"Cancel"</button>
                </div>
            </form>
        </Show>
    }
}

/// Names the version; a blank name takes its name away.
#[component]
fn NameVersion(
    id: u64,
    version: Memo<Option<Version>>,
    open: RwSignal<bool>,
    reload: Callback<()>,
) -> impl IntoView {
    let config = expect_context::<Room>().config.clone();
    let input = NodeRef::<Input>::new();
    let error: Error = RwSignal::new(None);
    Effect::new(move |_| {
        if let Some(input) = input.get() {
            let _ = input.focus();
        }
    });
    let initial = version.get_untracked().and_then(|version| version.name);
    let send = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let name = input.get().map(|input| input.value()).unwrap_or_default();
        let config = config.clone();
        spawn_local(async move {
            match api::name(&config, id, &name).await {
                Ok(()) => {
                    open.set(false);
                    reload.run(());
                }
                Err(failure) => error.set(Some(failure.to_string())),
            }
        });
    };
    view! {
        <form class="name-form version-name" on:submit=send>
            <label>
                <span class="field-label">"Name of this version (leave blank to remove it)"</span>
                <input type="text" maxlength="100" node_ref=input value=initial/>
            </label>
            {error_line(error)}
            <div class="row-buttons">
                <button type="submit" class="button primary">"Save name"</button>
                <button type="button" class="button" on:click=move |_| open.set(false)>"Cancel"</button>
            </div>
        </form>
    }
}

fn note(version: Memo<Option<Version>>) -> Option<String> {
    version.with(|version| version.as_ref()?.message.clone())
}

/// Saves the newest version with an optional name and note, then shows it.
#[component]
fn SaveVersion(
    open: RwSignal<bool>,
    scope: Memo<Option<String>>,
    reload: Callback<()>,
) -> impl IntoView {
    let config = expect_context::<Room>().config.clone();
    let name = NodeRef::<Input>::new();
    let note = NodeRef::<Textarea>::new();
    let error: Error = RwSignal::new(None);
    Effect::new(move |_| {
        if let Some(name) = name.get() {
            let _ = name.focus();
        }
    });
    let send = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let typed = name.get().map(|input| input.value()).unwrap_or_default();
        let noted = note.get().map(|input| input.value()).unwrap_or_default();
        let config = config.clone();
        spawn_local(async move {
            match api::save_version(&config, &typed, &noted).await {
                Ok(id) => {
                    if let Some(next) = after_save(open, scope, id) {
                        reload.run(());
                        go_to(&next);
                    }
                }
                Err(failure) => error.set(Some(failure.to_string())),
            }
        });
    };
    view! {
        <form class="name-form save-form" on:submit=send>
            <Hint anchor="history" topic="saving a version"
                text="The name shows in the list, like “paper v1”. The note says why, like “as submitted”."/>
            <label>
                <span class="field-label">"Name (optional)"</span>
                <input type="text" maxlength="100" node_ref=name/>
            </label>
            <label>
                <span class="field-label">"Note (optional)"</span>
                <textarea rows="3" maxlength="2000" node_ref=note></textarea>
            </label>
            {error_line(error)}
            <div class="row-buttons">
                <button type="submit" class="button primary">"Save"</button>
                <button type="button" class="button" on:click=move |_| open.set(false)>"Cancel"</button>
            </div>
        </form>
    }
}

type Past = LocalResource<Option<Result<Vec<Row>, String>>>;

/// The changes of one version: the counts first, then the events a page at a time.
#[component]
fn ChangesPanel(id: u64, scope: Memo<Option<String>>) -> impl IntoView {
    let config = expect_context::<Room>().config.clone();
    let only_changed = RwSignal::new(true);
    let first = {
        let config = config.clone();
        LocalResource::new(move || {
            let (config, catalogue) = (config.clone(), scope.get());
            async move {
                api::changes_page(&config, id, catalogue.as_deref(), None)
                    .await
                    .map_err(|error| error.to_string())
            }
        })
    };
    let past: Past = LocalResource::new(move || {
        let wanted = scope.get().filter(|_| !only_changed.get());
        let config = config.clone();
        async move {
            let catalogue = wanted?;
            let update = api::state(&config, id)
                .await
                .map_err(|error| error.to_string());
            Some(update.and_then(|update| past_rows(&update, &catalogue)))
        }
    });
    let table = move || {
        Some(match first.get()? {
            Err(error) => view! { <p class="field-error" role="alert">{error}</p> }.into_any(),
            Ok(page) => {
                let catalogue = scope.get_untracked();
                view! { <ChangesTable id catalogue page past only_changed/> }.into_any()
            }
        })
    };
    view! {
        <div class="page-head">
            <h1>"Changes in this version"</h1>
            <Show when=move || scope.with(Option::is_some)>
                <label class="switch">
                    <input type="checkbox" prop:checked=move || only_changed.get()
                        on:change=move |event| only_changed.set(event_target_checked(&event))/>
                    "Only show changed events"
                </label>
            </Show>
        </div>
        <Suspense fallback=|| view! { <p class="quiet">"Loading the changes…"</p> }>
            {table}
        </Suspense>
        <p class="quiet">
            "Compared with the project just before this version."
            {move || only_changed.get().then_some(" Unchanged events are hidden.")}
        </p>
    }
}

/// The unchanged events of a past catalogue among the changed rows, in time order.
fn with_unchanged(rows: &[DiffRow], past: &[Row]) -> Vec<DiffRow> {
    let mut all = rows.to_vec();
    all.extend(unchanged_rows(past, rows));
    all.sort_by_key(|row| row.start);
    all
}

/// The rows of `range` as table rows; the range is cut to the rows there are.
fn rows_in(rows: &[DiffRow], range: Range<usize>) -> Vec<AnyView> {
    let end = range.end.min(rows.len());
    let start = range.start.min(end);
    rows[start..end].iter().cloned().map(diff_row).collect()
}

/// One version's changes as first answered, and the pages that follow as the table nears its
/// end. Only the rows in view are in the page, like the event table.
#[component]
fn ChangesTable(
    id: u64,
    catalogue: Option<String>,
    page: ChangesPage,
    past: Past,
    only_changed: RwSignal<bool>,
) -> impl IntoView {
    let config = expect_context::<Room>().config.clone();
    let changes = RwSignal::new(Changes::first(page));
    let fetching = RwSignal::new(false);
    let failure = RwSignal::new(None::<String>);
    let scroller = NodeRef::<Div>::new();
    let scroll_top = RwSignal::new(0.0);
    let viewport = RwSignal::new(ROW * 16.0);
    let measure = move || {
        if let Some(scroller) = scroller.get() {
            scroll_top.set(f64::from(scroller.scroll_top()));
            viewport.set(f64::from(scroller.client_height()));
        }
    };
    Effect::new(move |_| measure());
    let merged = Memo::new(move |_| {
        if only_changed.get() {
            return None;
        }
        changes.with(|changes| {
            let past = past.with(|past| match past {
                Some(Some(Ok(past))) => Some(past.clone()),
                _ => None,
            })?;
            changes
                .next
                .is_none()
                .then(|| with_unchanged(&changes.rows, &past))
        })
    });
    let total = move || {
        merged.with(|merged| match merged {
            Some(rows) => rows.len(),
            None => changes.with(|changes| changes.rows.len()),
        })
    };
    let range = Memo::new(move |_| visible_rows(scroll_top.get(), viewport.get(), ROW, total()));
    Effect::new(move |_| {
        let view_end = range.get().end;
        // simplify: showing the unchanged events loads every page, which is inherent: merging
        // them in needs every changed id.
        let everything = !only_changed.get();
        let busy = fetching.get() || failure.with(Option::is_some);
        let (loaded, cursor) = changes.with(|c| (c.rows.len(), c.next.clone()));
        let Some(cursor) =
            cursor.filter(|cursor| !busy && wants_next(view_end, loaded, Some(cursor), everything))
        else {
            return;
        };
        fetching.set(true);
        let (config, catalogue) = (config.clone(), catalogue.clone());
        spawn_local(async move {
            let answer = api::changes_page(&config, id, catalogue.as_deref(), Some(&cursor)).await;
            fetching.set(false);
            match answer {
                Ok(page) => changes.update(|changes| changes.append(&cursor, page)),
                Err(error) => failure.set(Some(error.to_string())),
            }
        });
    });
    let body = move || {
        let range = range.get();
        let total = total();
        let rows = merged.with(|merged| match merged {
            Some(rows) => rows_in(rows, range.clone()),
            None => changes.with(|changes| rows_in(&changes.rows, range.clone())),
        });
        let end = range.end.min(total);
        view! {
            <tr class="spacer" style=format!("height: {}px", range.start.min(end) as f64 * ROW)></tr>
            {rows}
            <tr class="spacer" style=format!("height: {}px", (total - end) as f64 * ROW)></tr>
        }
    };
    let past_error = move || {
        past.with(|past| match past {
            Some(Some(Err(error))) => Some(error.clone()),
            _ => None,
        })
    };
    let summary = move || {
        changes.with(|changes| {
            let counts = &changes.counts;
            let lines = counts
                .catalogues
                .iter()
                .map(|row| format!("“{}”: {}", row.name, tally_text(&row.tally)))
                .collect::<Vec<_>>();
            view! {
                <p class="legend">{tally_text(&counts.total)}</p>
                <ul class="catalogue-changes">
                    {changes.catalogues.iter().map(|change| view! { <li>{catalogue_line(change)}</li> }).collect_view()}
                </ul>
                <ul class="catalogue-changes">
                    {lines.into_iter().map(|line| view! { <li>{line}</li> }).collect_view()}
                </ul>
            }
        })
    };
    view! {
        {summary}
        {move || past_error().map(|error| view! { <p class="field-error" role="alert">{error}</p> })}
        <div class="table-scroll" node_ref=scroller on:scroll=move |_| measure()>
            <table class="events diff">
                {diff_head()}
                <tbody>{body}</tbody>
            </table>
        </div>
        {move || failure.get().map(|error| view! {
            <p class="field-error" role="alert">
                {error} " "
                <button type="button" class="link-button" on:click=move |_| failure.set(None)>"Try again"</button>
            </p>
        })}
        <Show when=move || fetching.get()>
            <p class="quiet">"Loading more changes…"</p>
        </Show>
    }
}

/// The counts with the colours of their rows.
pub(crate) fn legend(added: u32, changed: u32, removed: u32) -> impl IntoView {
    if counts_text(added, changed, removed).is_none() {
        return view! { <span>"No event changed"</span> }.into_any();
    }
    [(added, "added"), (changed, "changed"), (removed, "removed")]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, word)| view! { <span class=word>{format!("{count} {word}")}</span> })
        .collect_view()
        .into_any()
}

#[component]
pub(crate) fn DiffTable(rows: Vec<DiffRow>) -> impl IntoView {
    view! {
        <div class="table-scroll diff-scroll">
            <table class="events diff">
                {diff_head()}
                <tbody>{rows.into_iter().map(diff_row).collect_view()}</tbody>
            </table>
        </div>
    }
}

fn diff_head() -> impl IntoView {
    view! {
        <colgroup>
            <col class="w-mark"/>
            <col class="w-start"/>
            <col class="w-duration"/>
            <col class="w-tags"/>
            <col class="w-confidence"/>
            <col class="w-status"/>
            <col/>
        </colgroup>
        <thead>
            <tr>
                <th scope="col"><span class="sr-only">"Change"</span></th>
                {["Start", "Duration", "Tags", "Confidence", "Status", "Also changed"]
                    .map(|label| view! { <th scope="col">{label}</th> })}
            </tr>
        </thead>
    }
}

fn diff_row(row: DiffRow) -> AnyView {
    let (class, mark, label) = match row.change {
        Some(Change::Added) => ("added", "+", "Added"),
        Some(Change::Changed) => ("changed", "~", "Changed"),
        Some(Change::Removed) => ("removed", "−", "Removed"),
        None => ("same", "", "Unchanged"),
    };
    let [start, duration, tags, confidence, status] = row.cells;
    view! {
        <tr class=format!("diff-row {class}")>
            <td class="mark" title=label>{mark}</td>
            {cell(start, true)}
            {cell(duration, true)}
            {cell(tags, false)}
            {cell(confidence, true)}
            {cell(status, false)}
            <td class="quiet-cell">{row.also.join(", ")}</td>
        </tr>
    }
    .into_any()
}

fn cell(cell: Cell, mono: bool) -> impl IntoView {
    let class = if mono { "mono" } else { "" };
    match cell {
        Cell::Same(text) => {
            let title = text.clone();
            view! { <td class=class title=title>{text}</td> }.into_any()
        }
        Cell::Struck(text) => {
            let title = text.clone();
            view! { <td class=class title=title><s>{text}</s></td> }.into_any()
        }
        Cell::Note(text) => view! { <td class="note">{text}</td> }.into_any(),
        Cell::Changed { before, after } => {
            let title = format!("{before} → {after}");
            view! {
                <td class=class title=title>
                    <s class="old">{before}</s>" → "<span class="new">{after}</span>
                </td>
            }
            .into_any()
        }
    }
}

#[component]
fn VersionList(
    versions: Loaded,
    scope: Memo<Option<String>>,
    picked: Memo<Option<u64>>,
    reload: Callback<()>,
) -> impl IntoView {
    let named_only = RwSignal::new(false);
    let saving = RwSignal::new(false);
    let editor = expect_context::<Room>().can_write();
    let page = page();
    let days = move || {
        let Some(versions) = versions.get() else {
            return Some(view! { <p class="quiet pad">"Loading the history…"</p> }.into_any());
        };
        Some(match versions {
            Err(error) => view! { <p class="field-error" role="alert">{error}</p> }.into_any(),
            Ok(versions) => {
                let current = versions.first().map(|version| version.id);
                let kept: Vec<Version> = versions
                    .into_iter()
                    .filter(|version| !named_only.get() || version.name.is_some())
                    .collect();
                if kept.is_empty() {
                    return Some(
                        view! { <p class="quiet pad">"No versions to show."</p> }.into_any(),
                    );
                }
                let days = by_day(&kept, js_sys::Date::now() as i64, offset);
                let page = page.clone();
                let scope = scope.get();
                days.into_iter()
                    .map(|day| day_view(day, current, &scope, picked, &page))
                    .collect_view()
                    .into_any()
            }
        })
    };
    view! {
        <aside class="versions" aria-label="Version history">
            <div class="versions-head">
                <h2>"Version history"</h2>
                <label class="switch small">
                    <input type="checkbox" prop:checked=move || named_only.get()
                        on:change=move |event| named_only.set(event_target_checked(&event))/>
                    "Named only"
                </label>
            </div>
            <Hint anchor="history" topic="history"
                text="A version is one person's run of changes. A 5-minute pause, an hour, or Save version starts the next one."/>
            {editor.then(|| view! {
                <div class="save-version">
                    <Show when=move || !saving.get()>
                        <button type="button" class="button" on:click=move |_| saving.set(true)>
                            "Save version"
                        </button>
                    </Show>
                    <Show when=move || saving.get()>
                        <SaveVersion open=saving scope reload/>
                    </Show>
                </div>
            })}
            {days}
        </aside>
    }
}

fn day_view(
    day: Day,
    current: Option<u64>,
    scope: &Option<String>,
    picked: Memo<Option<u64>>,
    page: &str,
) -> impl IntoView {
    let items = day
        .versions
        .into_iter()
        .map(|version| version_item(version, current, scope, picked, page))
        .collect_view();
    view! {
        <h3 class="day">{day.label}</h3>
        <ul class="version-list">{items}</ul>
    }
}

fn version_item(
    version: Version,
    current: Option<u64>,
    scope: &Option<String>,
    picked: Memo<Option<u64>>,
    page: &str,
) -> impl IntoView {
    let id = version.id;
    let href = history(scope.clone(), Some(id)).href(page);
    let picked = move || (picked.get() == Some(id)).then_some("true");
    let time = match (current == Some(id), version.ended_ms) {
        (true, _) => "Current".to_owned(),
        (false, Some(ended)) => clock(ended, offset(ended)),
        (false, None) => String::new(),
    };
    let title = version
        .ended_ms
        .map(|ended| when_text(ended, offset(ended)));
    view! {
        <li>
            <a class="version" href=href aria-current=picked title=title>
                <span class="version-top">
                    <span class="time">
                        {time}
                        {version.committed.then(SavedMark)}
                    </span>
                    {version.name.clone().map(|name| view! { <span class="chip named">{name}</span> })}
                </span>
                <span class="who" title=version.author_email.clone()>
                    <Dot version=version.clone()/>
                    {author(&version)}
                </span>
                <span class="summary">{summary(&version, offset)}</span>
            </a>
        </li>
    }
}

/// The author's colour, as presence shows the account.
#[component]
fn Dot(version: Version) -> impl IntoView {
    let color = version
        .author_color
        .unwrap_or_else(|| "var(--muted)".to_owned());
    view! { <span class="author-dot" style=format!("background: {color}") aria-hidden="true"></span> }
}

/// The latest versions that touched an event, each a link into the history of what is on
/// screen. Read when the panel opens on the event.
#[component]
pub fn LastChanges(uuid: String, shown: Memo<Route>) -> impl IntoView {
    let config = expect_context::<Room>().config.clone();
    let versions: Versions = LocalResource::new(move || {
        let (config, uuid) = (config.clone(), uuid.clone());
        async move {
            api::history(&config, None, Some(&uuid))
                .await
                .map_err(|error| error.to_string())
        }
    });
    let page = page();
    let list = move || {
        let versions = versions.get()?;
        let catalogue = match shown.get_untracked() {
            Route::Catalogue(uuid) => Some(uuid),
            _ => None,
        };
        Some(match versions {
            Err(error) => view! { <p class="quiet">{error}</p> }.into_any(),
            Ok(versions) if versions.is_empty() => {
                view! { <p class="quiet">"No change since the history began."</p> }.into_any()
            }
            Ok(versions) => {
                let items = versions
                    .into_iter()
                    .take(LAST_CHANGES)
                    .map(|version| {
                        let href = history(catalogue.clone(), Some(version.id)).href(&page);
                        let when = version
                            .ended_ms
                            .map(|ended| when_text(ended, offset(ended)))
                            .unwrap_or_else(|| "Before history".to_owned());
                        view! {
                            <li>
                                <a href=href>
                                    <Dot version=version.clone()/>
                                    <span class="when">{when}</span>
                                </a>
                                <span class="quiet">{format!("{} · {}", author(&version), summary(&version, offset))}</span>
                            </li>
                        }
                    })
                    .collect_view();
                view! { <ul class="plain last-changes">{items}</ul> }.into_any()
            }
        })
    };
    view! {
        <section>
            <h3 class="section-label">"Last changes"</h3>
            <Suspense fallback=|| view! { <p class="quiet">"Loading…"</p> }>{list}</Suspense>
        </section>
    }
}

/// Marks a version someone saved on purpose.
#[component]
fn SavedMark() -> impl IntoView {
    view! {
        <span class="saved-mark" role="img" aria-label="Saved version" title="Saved version">
            <svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                <path d="M6 3h12a1 1 0 0 1 1 1v17l-7-4.5L5 21V4a1 1 0 0 1 1-1z"></path>
            </svg>
        </span>
    }
}

#[component]
fn HistoryIcon() -> impl IntoView {
    view! {
        <svg class="history-icon" width="18" height="18" viewBox="0 0 24 24" fill="none"
            stroke="currentColor" stroke-width="2" aria-hidden="true">
            <path d="M3 12a9 9 0 1 0 3-6.7L3 8"></path>
            <path d="M3 3v5h5M12 7v5l3 2"></path>
        </svg>
    }
}

/// Where the page goes once a version is saved: the version, in the scope it was saved
/// from. `None` when the form went away while the server answered (the user left the
/// history or switched project): its signals are gone, and reading one would panic.
fn after_save(open: RwSignal<bool>, scope: Memo<Option<String>>, id: u64) -> Option<String> {
    let catalogue = scope.try_get_untracked()?;
    let closed = open.try_set(false).is_some();
    (!closed).then(|| history(catalogue, Some(id)).hash())
}

fn history(catalogue: Option<String>, version: Option<u64>) -> Route {
    Route::History { catalogue, version }
}

fn page() -> String {
    window().location().pathname().unwrap_or_default()
}

/// The visitor's offset from UTC, in minutes, at an instant.
pub(crate) fn offset(ms: i64) -> i32 {
    let date = js_sys::Date::new(&JsValue::from_f64(ms as f64));
    -(date.get_timezone_offset() as i32)
}
