//! The history: the versions of what was on screen, what one of them changed, and, for
//! editors, naming and restoring it. Also the event panel's latest changes.

use leptos::html::Input;
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsValue;

use crate::api;
use crate::history::{
    Cell, Change, Day, DiffRow, Version, author, by_day, catalogue_line, clock, counts_text,
    diff_counts, diff_rows, past_rows, summary, unchanged_rows, when_text,
};
use crate::route::Route;
use crate::state::Room;
use crate::views::controls::{Error, confirm, error_line, go_to};

/// Unchanged events shown at most below a version's changes.
// simplify: a plain table, cut at this many rows; the live table's virtualised rows would
// show a 100k-event catalogue whole if people need to scroll it all.
const MOST_ROWS: usize = 2_000;
/// The event panel's latest changes.
const LAST_CHANGES: usize = 5;

type Versions = LocalResource<Result<Vec<Version>, String>>;

/// `scope` (the catalogue, else the room) and `picked` (the version) belong to the page
/// around this view and keep their values when it leaves the history, so nothing here
/// reloads while it is taken down.
#[component]
pub fn HistoryPage(scope: Memo<Option<String>>, picked: Memo<Option<u64>>) -> impl IntoView {
    let room = expect_context::<Room>();
    let reload = RwSignal::new(0_u32);
    let config = room.config.clone();
    let versions: Versions = LocalResource::new(move || {
        reload.track();
        let (config, catalogue) = (config.clone(), scope.get());
        async move {
            api::history(&config, catalogue.as_deref(), None)
                .await
                .map_err(|error| error.to_string())
        }
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
                    Some(id) => view! { <Changes id scope/> }.into_any(),
                    None => view! {
                        <div class="page-head"><h1>"History"</h1></div>
                        <p class="quiet">"Pick a version to see what it changed."</p>
                    }.into_any(),
                }}
            </main>
            <VersionList versions scope picked/>
        </div>
    }
}

/// What is on screen, and the read-only notice with "Back to now" and, for editors,
/// naming and restoring.
#[component]
fn Banner(
    id: u64,
    version: Memo<Option<Version>>,
    scope: Memo<Option<String>>,
    reload: RwSignal<u32>,
) -> impl IntoView {
    let room = expect_context::<Room>();
    let page = page();
    let reader = room.clone();
    let subject = Memo::new(move |_| match scope.get() {
        Some(uuid) => reader
            .read(|txn| cocat_core::model::catalogue(txn, &uuid))
            .map_or_else(|| "this catalogue".to_owned(), |catalogue| catalogue.name),
        None => reader.config.room_name.clone(),
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
            <p>
                "You are looking at " <strong>{move || subject.get()}</strong>
                {move || when().map(|when| view! { " as it was on " <strong>{when}</strong> })}
                ", " {after} ". It is read-only."
            </p>
            <div class="banner-buttons">
                <a class="button" href=live>"Back to now"</a>
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
    }
}

/// Names the version; a blank name takes its name away.
#[component]
fn NameVersion(
    id: u64,
    version: Memo<Option<Version>>,
    open: RwSignal<bool>,
    reload: RwSignal<u32>,
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
                    reload.update(|count| *count += 1);
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

/// The changes table of one version.
#[component]
fn Changes(id: u64, scope: Memo<Option<String>>) -> impl IntoView {
    let config = expect_context::<Room>().config.clone();
    let only_changed = RwSignal::new(true);
    let diff = {
        let config = config.clone();
        LocalResource::new(move || {
            let (config, catalogue) = (config.clone(), scope.get());
            async move {
                api::changes(&config, id, catalogue.as_deref())
                    .await
                    .map_err(|error| error.to_string())
            }
        })
    };
    let past = LocalResource::new(move || {
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
        let diff = diff.get()?;
        let past = past.get().flatten();
        Some(match (diff, past) {
            (Err(error), _) | (_, Some(Err(error))) => {
                view! { <p class="field-error" role="alert">{error}</p> }.into_any()
            }
            (Ok(diff), past) => {
                let (added, changed, removed) = diff_counts(&diff);
                let mut rows = diff_rows(&diff);
                if let Some(Ok(past)) = past {
                    rows.extend(unchanged_rows(&past, &diff));
                    rows.sort_by_key(|row| row.start);
                }
                let total = rows.len();
                rows.truncate(MOST_ROWS);
                view! {
                    <p class="legend">{legend(added, changed, removed)}</p>
                    <ul class="catalogue-changes">
                        {diff.catalogues.iter().map(|change| view! { <li>{catalogue_line(change)}</li> }).collect_view()}
                    </ul>
                    <DiffTable rows/>
                    {(total > MOST_ROWS).then(|| view! {
                        <p class="quiet">{format!("Showing the first {MOST_ROWS} of {total} events.")}</p>
                    })}
                }
                .into_any()
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
            "Compared with the room just before this version."
            {move || only_changed.get().then_some(" Unchanged events are hidden.")}
        </p>
    }
}

/// The counts with the colours of their rows.
fn legend(added: u32, changed: u32, removed: u32) -> impl IntoView {
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
fn DiffTable(rows: Vec<DiffRow>) -> impl IntoView {
    view! {
        <div class="table-scroll diff-scroll">
            <table class="events diff">
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
                <tbody>{rows.into_iter().map(diff_row).collect_view()}</tbody>
            </table>
        </div>
    }
}

fn diff_row(row: DiffRow) -> impl IntoView {
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
    versions: Versions,
    scope: Memo<Option<String>>,
    picked: Memo<Option<u64>>,
) -> impl IntoView {
    let named_only = RwSignal::new(false);
    let page = page();
    let days = move || {
        let versions = versions.get()?;
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
            <Suspense fallback=|| view! { <p class="quiet pad">"Loading the history…"</p> }>
                {days}
            </Suspense>
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
                    <span class="time">{time}</span>
                    {version.name.clone().map(|name| view! { <span class="chip named">{name}</span> })}
                </span>
                <span class="who" title=version.author_email.clone()>
                    <Dot version=version.clone()/>
                    {author(&version)}
                </span>
                <span class="summary">{summary(&version)}</span>
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
                                <span class="quiet">{format!("{} · {}", author(&version), summary(&version))}</span>
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

fn history(catalogue: Option<String>, version: Option<u64>) -> Route {
    Route::History { catalogue, version }
}

fn page() -> String {
    window().location().pathname().unwrap_or_default()
}

/// The visitor's offset from UTC, in minutes, at an instant.
fn offset(ms: i64) -> i32 {
    let date = js_sys::Date::new(&JsValue::from_f64(ms as f64));
    -(date.get_timezone_offset() as i32)
}
