//! The page for one catalogue or the trash: heading, filter, timeline and table.

use leptos::prelude::*;

use crate::help::{Shown, empty_text};
use crate::period::Period;
use crate::route::Route;
use crate::rows::{Row, SortKey, attribute_keys, filter, rows, sort, trash_rows};
use crate::state::Room;
use crate::views::actions::{Actions, Form, Freeze};
use crate::views::controls::Hint;
use crate::views::rule::Heading;
use crate::views::rule_editor::RuleEditor;
use crate::views::table::Table;
use crate::views::timeline::{PeriodLine, Timeline};
use crate::views::trash::TrashedCatalogues;

/// The rows of what is on screen, or the rule's error.
pub type Members = Memo<Result<Vec<Row>, String>>;

/// The events the table shows, before sorting, for the panel's statistics.
#[derive(Clone, Copy)]
pub struct OnScreen {
    pub members: Members,
    pub visible: Memo<Vec<usize>>,
}

impl OnScreen {
    /// `of` the rows on screen, and how many the page holds in all.
    pub fn with<T>(&self, of: impl FnOnce(&[&Row], usize) -> T) -> T {
        self.members.with(|members| {
            let all = members.as_deref().unwrap_or_default();
            self.visible.with(|visible| {
                let rows: Vec<&Row> = visible.iter().map(|&index| &all[index]).collect();
                of(&rows, all.len())
            })
        })
    }
}

#[component]
pub fn CataloguePage(
    shown: Memo<Route>,
    selected: RwSignal<Option<Row>>,
    screen: RwSignal<Option<OnScreen>>,
) -> impl IntoView {
    let room = expect_context::<Room>();
    let reader = room.clone();
    let members: Members = Memo::new(move |_| match shown.get() {
        Route::Catalogue(uuid) => reader
            .read(|txn| rows(txn, &uuid))
            .map_err(|error| error.to_string()),
        Route::Trash => Ok(reader.read(|txn| trash_rows(txn))),
        Route::Default | Route::History { .. } | Route::Groups | Route::Group(_) => Ok(Vec::new()),
    });
    let query = RwSignal::new(String::new());
    let period = RwSignal::new(None::<Period>);
    let sorting = RwSignal::new((SortKey::Start, false));
    let matching = Memo::new(move |_| {
        members.with(|members| match members {
            Ok(rows) => query.with(|query| filter(rows, query)),
            Err(_) => Vec::new(),
        })
    });
    let visible = Memo::new(move |_| {
        members.with(|members| {
            let Ok(rows) = members else {
                return Vec::new();
            };
            let mut visible = matching.get();
            if let Some(period) = period.get() {
                visible.retain(|&index| period.overlaps(rows[index].start, rows[index].stop));
            }
            visible
        })
    });
    screen.set(Some(OnScreen { members, visible }));
    on_cleanup(move || screen.set(None));
    let order = Memo::new(move |_| {
        members.with(|members| {
            let Ok(rows) = members else {
                return Vec::new();
            };
            let mut order = visible.get();
            sorting.with(|(key, descending)| sort(rows, &mut order, key, *descending));
            order
        })
    });
    // The columns the visitor picked here, else the ones the catalogue asks for.
    let picked = RwSignal::new(None::<Vec<String>>);
    let reader = room.clone();
    let columns = Memo::new(move |_| {
        picked
            .get()
            .unwrap_or_else(|| preferred_columns(&reader, &shown.get()))
    });
    let form = RwSignal::new(Form::Closed);
    Effect::new(move |_| {
        shown.track();
        form.set(Form::Closed);
        picked.set(None);
        query.set(String::new());
        period.set(None);
        selected.set(None);
    });
    let can_write = room.can_write();
    let reader = room.clone();
    let empty = Signal::derive(move || {
        let filtered = members.with(|members| members.as_ref().is_ok_and(|rows| !rows.is_empty()));
        empty_text(showing(&reader, &shown.get()), filtered, can_write)
    });
    let keys = Memo::new(move |_| {
        members.with(|members| {
            members
                .as_ref()
                .map(|rows| attribute_keys(rows))
                .unwrap_or_default()
        })
    });
    view! {
        <div class="page-head">
            <Heading shown members order/>
            <div class="tools">
                <label class="search">
                    <SearchIcon/>
                    <input
                        type="search"
                        placeholder="Filter events"
                        aria-label="Filter events"
                        prop:value=move || query.get()
                        on:input=move |event| query.set(event_target_value(&event))
                    />
                </label>
                <ColumnPicker keys columns picked/>
                {room.can_write().then(|| view! { <Actions shown selected form/> })}
            </div>
        </div>
        {move || match (form.get(), shown.get()) {
            (Form::Rule, Route::Catalogue(uuid)) => Some(view! { <RuleEditor uuid form/> }.into_any()),
            (Form::Freeze, Route::Catalogue(uuid)) => Some(view! { <Freeze uuid form/> }.into_any()),
            _ => None,
        }}
        {move || (shown.get() == Route::Trash).then(|| view! {
            <Hint anchor="catalogues" topic="the trash"
                text="Deleted events and catalogues wait here. Restore puts one back; Delete for good cannot be undone."/>
            <TrashedCatalogues/>
        })}
        <Timeline members matching period selected/>
        <PeriodLine period order matching/>
        <Table members order sorting columns selected empty/>
    }
}

fn showing(room: &Room, shown: &Route) -> Shown {
    match shown {
        Route::Catalogue(uuid) => Shown::Catalogue {
            smart: room
                .read(|txn| cocat_core::model::catalogue(txn, uuid))
                .is_some_and(|catalogue| catalogue.rule.is_some()),
        },
        Route::Trash => Shown::Trash,
        Route::Default | Route::History { .. } | Route::Groups | Route::Group(_) => {
            Shown::NoCatalogue
        }
    }
}

/// The attribute columns a catalogue asks to show.
fn preferred_columns(room: &Room, shown: &Route) -> Vec<String> {
    match shown {
        Route::Catalogue(uuid) => room
            .read(|txn| cocat_core::model::catalogue(txn, uuid))
            .map(|catalogue| catalogue.columns)
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

#[component]
fn ColumnPicker(
    keys: Memo<Vec<String>>,
    columns: Memo<Vec<String>>,
    picked: RwSignal<Option<Vec<String>>>,
) -> impl IntoView {
    let toggle = move |key: String| {
        let mut shown = columns.get_untracked();
        match shown.iter().position(|column| *column == key) {
            Some(index) => {
                shown.remove(index);
            }
            None => shown.push(key),
        }
        picked.set(Some(shown));
    };
    view! {
        <Show when=move || keys.with(|keys| !keys.is_empty())>
            <details class="columns">
                <summary>"Columns"</summary>
                <div class="menu">
                    <For each=move || keys.get() key=Clone::clone let:key>
                        <label>
                            <input
                                type="checkbox"
                                prop:checked={
                                    let key = key.clone();
                                    move || columns.with(|columns| columns.contains(&key))
                                }
                                on:change={
                                    let key = key.clone();
                                    move |_| toggle(key.clone())
                                }
                            />
                            {key.clone()}
                        </label>
                    </For>
                </div>
            </details>
        </Show>
    }
}

#[component]
fn SearchIcon() -> impl IntoView {
    view! {
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
            stroke-width="2" aria-hidden="true">
            <circle cx="11" cy="11" r="7"></circle>
            <path d="M20 20l-3.5-3.5"></path>
        </svg>
    }
}
