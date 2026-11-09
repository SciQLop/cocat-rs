//! The page for one catalogue or the trash: heading, filter, timeline and table.

use leptos::prelude::*;

use crate::help::{Shown, empty_text};
use crate::period::Period;
use crate::route::Route;
use crate::rows::{
    Row, SortKey, attribute_keys, filter, matching, rows, search_rows, sort, trash_rows,
};
use crate::search::{self, Query};
use crate::state::Room;
use crate::submissions::{Proposal, proposal_for};
use crate::views::actions::{Actions, Form, Freeze, PublicButtons};
use crate::views::controls::Hint;
use crate::views::rule::Heading;
use crate::views::rule_editor::RuleEditor;
use crate::views::search::{SearchBuilder, SearchLine, saved_search};
use crate::views::submissions::{CopyForm, ProposeForm, SubmitForm, UpdateForm};
use crate::views::table::Table;
use crate::views::timeline::{PeriodLine, Timeline};
use crate::views::transfer::ExportForm;
use crate::views::trash::TrashedCatalogues;
use cocat_core::room_id::is_public_id;
use leptos::task::spawn_local;

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
    let query = RwSignal::new(String::new());
    let building = RwSignal::new(false);
    let reader = room.clone();
    let saved = Memo::new(move |_| saved_search(&reader, &shown.get()));
    let reader = room.clone();
    let parsed = Memo::new(move |_| {
        let names = reader.read(|txn| cocat_core::model::catalogue_names(txn));
        query.with(|text| search::read(text, &names))
    });
    // A smart catalogue's search, once edited, shows what it would hold before it is saved.
    let preview = move || {
        let source = saved.with(|saved| {
            saved
                .as_ref()
                .filter(|saved| query.with(|text| text.trim() != saved.text))
                .map(|saved| saved.source.clone())
        })?;
        let text = query.get();
        Some((text, source))
    };
    let reader = room.clone();
    let members: Members = Memo::new(move |_| match (shown.get(), preview()) {
        (Route::Catalogue(_), Some((text, source))) => reader.read(|txn| {
            let names = cocat_core::model::catalogue_names(txn);
            match cocat_core::query::parse(&text, &names) {
                Ok(predicate) => search_rows(txn, &predicate, source.as_deref())
                    .map_err(|error| error.to_string()),
                Err(_) => Ok(Vec::new()),
            }
        }),
        (Route::Catalogue(uuid), None) => reader
            .read(|txn| rows(txn, &uuid))
            .map_err(|error| error.to_string()),
        (Route::Trash, _) => Ok(reader.read(|txn| trash_rows(txn))),
        _ => Ok(Vec::new()),
    });
    let period = RwSignal::new(None::<Period>);
    let sorting = RwSignal::new((SortKey::Start, false));
    let reader = room.clone();
    let matching = Memo::new(move |_| {
        members.with(|members| {
            let Ok(rows) = members else {
                return Vec::new();
            };
            let everything = || (0..rows.len()).collect();
            if saved.with(Option::is_some) {
                return everything();
            }
            parsed.with(|parsed| match parsed {
                Query::Words => query.with(|query| filter(rows, query)),
                Query::Rule(predicate) => reader
                    .read(|txn| matching(txn, rows, predicate))
                    .unwrap_or_else(|_| everything()),
                Query::Empty | Query::Broken(_) => everything(),
            })
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
    let public = is_public_id(&room.config.room_id);
    let public_id = room.config.room_id.clone();
    let proposals = copies_of(&room, public);
    Effect::new(move |_| {
        shown.track();
        form.set(Form::Closed);
        picked.set(None);
        building.set(false);
        period.set(None);
        selected.set(None);
    });
    // The box holds the smart catalogue's search, again once it is saved.
    Effect::new(move |_| {
        shown.track();
        let text = saved.with(|saved| saved.as_ref().map(|saved| saved.text.clone()));
        query.set(text.unwrap_or_default());
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
                        placeholder="Search events"
                        aria-label="Search events"
                        title="Words, or fields: tag:shock confidence:>=0.8 start:2015 created:now-7d"
                        prop:value=move || query.get()
                        on:input=move |event| query.set(event_target_value(&event))
                    />
                    <button type="button" class="build-search" aria-label="Build a search"
                        title="Build a search" aria-pressed=move || building.get().to_string()
                        on:click=move |_| building.update(|open| *open = !*open)>
                        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                            stroke-width="2" aria-hidden="true">
                            <path d="M4 6h10M18 6h2M4 12h4M12 12h8M4 18h12M20 18h0"/>
                            <circle cx="16" cy="6" r="2"/><circle cx="10" cy="12" r="2"/><circle cx="18" cy="18" r="2"/>
                        </svg>
                    </button>
                </label>
                <ColumnPicker keys columns picked/>
                <Actions shown selected form proposals/>
                {public.then(|| view! { <PublicButtons shown form editor=can_write/> })}
            </div>
        </div>
        <SearchLine shown query parsed saved/>
        <Show when=move || building.get()>
            <SearchBuilder query building keys/>
        </Show>
        {move || match (form.get(), shown.get()) {
            (Form::Rule, Route::Catalogue(uuid)) => Some(view! { <RuleEditor uuid form/> }.into_any()),
            (Form::Freeze, Route::Catalogue(uuid)) => Some(view! { <Freeze uuid form/> }.into_any()),
            (Form::Submit, Route::Catalogue(uuid)) => Some(view! { <SubmitForm uuid form/> }.into_any()),
            (Form::Update, Route::Catalogue(uuid)) => proposals
                .with(|listed| proposal_for(listed, &uuid).cloned())
                .map(|proposal| view! { <UpdateForm uuid proposal form/> }.into_any()),
            (Form::Propose, Route::Catalogue(_)) => Some(view! { <ProposeForm form/> }.into_any()),
            (Form::Copy, Route::Catalogue(_)) => {
                let close = Callback::new(move |()| form.set(Form::Closed));
                Some(view! { <CopyForm public=public_id.clone() close/> }.into_any())
            }
            (Form::Export, Route::Catalogue(uuid)) => Some(view! { <ExportForm uuid form/> }.into_any()),
            _ => None,
        }}
        {move || (shown.get() == Route::Trash).then(|| view! {
            <Hint anchor="catalogues" topic="the trash"
                text="Deleted events and catalogues wait here. Restore puts one back; Delete for good cannot be undone."/>
            <TrashedCatalogues/>
        })}
        <Timeline members matching period selected/>
        <PeriodLine period order members/>
        <Table members order sorting columns selected empty/>
    }
}

/// The catalogues of this project copied out of public catalogues, read once: an editor's
/// More menu offers to submit each as an update. A public project holds none.
fn copies_of(room: &Room, public: bool) -> RwSignal<Vec<Proposal>> {
    let proposals = RwSignal::new(Vec::new());
    if public || !room.can_write() {
        return proposals;
    }
    let (api, id) = (room.config.api_url.clone(), room.config.room_id.clone());
    spawn_local(async move {
        let path = ["rooms", id.as_str(), "proposals"];
        if let Ok(listed) = crate::api::read::<Vec<Proposal>>(&api, &path).await {
            proposals.try_set(listed);
        }
    });
    proposals
}

fn showing(room: &Room, shown: &Route) -> Shown {
    match shown {
        Route::Catalogue(uuid) => Shown::Catalogue {
            smart: room
                .read(|txn| cocat_core::model::catalogue(txn, uuid))
                .is_some_and(|catalogue| catalogue.rule.is_some()),
        },
        Route::Trash => Shown::Trash,
        _ => Shown::NoCatalogue,
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
