//! The page for one catalogue or the trash: heading, filter, timeline and table.

use leptos::prelude::*;

use crate::views::preview::{PreviewView, load_preview};
use crate::views::proposal::InProposal;

use crate::help::{Shown, empty_text, offers_public};
use crate::hits::Hits;
use crate::period::Period;
use crate::preview::{PreviewState, Stage, TableState, stage, switch_state};
use crate::public_events::{self, Found};
use crate::route::Route;
use crate::rows::{
    Row, SortKey, all_rows, attribute_keys, filter, matching, preview_rows, rows, sort, trash_rows,
};
use crate::search::{self, Listing, Query, Side, listing, public_query};
use crate::state::Room;
use crate::submissions::{Proposal, proposal_for};
use crate::sync::Loading;
use crate::views::actions::{Actions, Form, Freeze, PublicButtons, RenameForm};
use crate::views::controls::Hint;
use crate::views::projects::Projects;
use crate::views::rule::{Heading, live_about, live_counts};
use crate::views::rule_editor::RuleEditor;
use crate::views::search::{Boxed, SearchBox, SearchBuilder, SearchLine, own_search};
use crate::views::submissions::{CopyForm, ProposeForm, SubmitForm, UpdateForm};
use crate::views::table::Table;
use crate::views::timeline::{PeriodLine, Timeline};
use crate::views::transfer::ExportForm;
use crate::views::trash::TrashedCatalogues;
use cocat_core::room_id::is_public_id;
use leptos::task::spawn_local;

/// The rows of what is on screen, or the rule's error: from the project, or from the
/// server's pages while the project downloads.
#[derive(Clone, Copy)]
pub enum Members {
    Live(Memo<Result<Vec<Row>, String>>),
    Preview(RwSignal<Option<PreviewState>>),
}

impl Members {
    pub fn with<T>(&self, read: impl FnOnce(Result<&[Row], &str>) -> T) -> T {
        match self {
            Members::Live(members) => {
                members.with(|members| read(members.as_deref().map_err(String::as_str)))
            }
            Members::Preview(state) => {
                state.with(|state| read(Ok(state.as_ref().map_or(&[], |state| &state.rows))))
            }
        }
    }
}

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
            let all = members.unwrap_or_default();
            self.visible.with(|visible| {
                let rows: Vec<&Row> = visible.iter().map(|&index| &all[index]).collect();
                of(&rows, all.len())
            })
        })
    }
}

/// The catalogue on screen, once the project is loaded; until then its first pages from the
/// server, else how far loading got.
#[component]
pub fn CataloguePage(
    shown: Memo<Route>,
    selected: RwSignal<Option<Row>>,
    screen: RwSignal<Option<OnScreen>>,
) -> impl IntoView {
    let room = expect_context::<Room>();
    let loading = room.loading;
    let preview = RwSignal::new(None::<PreviewState>);
    let preview_scroll = RwSignal::new(0.0);
    let preview_period = RwSignal::new(None::<Period>);
    load_preview(
        &room,
        shown,
        preview,
        (preview_scroll, selected, preview_period),
    );
    let stage = Memo::new(move |_| {
        let first_page = preview.with(Option::is_some);
        loading.with(|loading| stage(loading.as_ref(), first_page))
    });
    move || {
        match stage.get() {
        Stage::Waiting => view! { <LoadingNote loading/> }.into_any(),
        Stage::Preview => {
            view! { <PreviewView state=preview selected scroll_top=preview_scroll period=preview_period/> }.into_any()
        }
        Stage::Live => {
            let kept = selected.with_untracked(|selected| {
                let (scroll, period) = (preview_scroll.get_untracked(), preview_period.get_untracked());
                switch_state(scroll, selected.as_ref(), period)
            });
            view! { <CatalogueView shown selected screen kept/> }.into_any()
        }
    }
    }
}

#[component]
fn LoadingNote(loading: RwSignal<Option<Loading>>) -> impl IntoView {
    view! {
        <div class="loading" role="status">
            <span class="spinner" aria-hidden="true"></span>
            <span>{move || loading.get().map(Loading::text)}</span>
        </div>
    }
}

/// `kept` is where the preview left the visitor, if one showed.
#[component]
fn CatalogueView(
    shown: Memo<Route>,
    selected: RwSignal<Option<Row>>,
    screen: RwSignal<Option<OnScreen>>,
    kept: TableState,
) -> impl IntoView {
    let room = expect_context::<Room>();
    if let Some(uuid) = kept.selected {
        selected.set(room.read_untracked(|txn| crate::rows::event(txn, &uuid)));
    }
    let scroll_top = RwSignal::new(kept.scroll_top);
    let boxed = Boxed::new();
    let query = boxed.query;
    let building = RwSignal::new(false);
    let projects = expect_context::<Projects>();
    if projects.public.with_untracked(Option::is_none) {
        projects.reload_public(|_| {});
    }
    let public_names = Memo::new(move |_| {
        projects
            .public
            .with(|all| all.as_deref().map(public_events::names).unwrap_or_default())
    });
    let reader = room.clone();
    let own =
        Memo::new(move |_| public_names.with(|public| own_search(&reader, &shown.get(), public)));
    let reader = room.clone();
    let parsed = Memo::new(move |_| {
        let names = match boxed.side.get() {
            Side::Public => public_names.get(),
            _ => reader.read(|txn| cocat_core::model::catalogue_names(txn)),
        };
        query.with(|text| search::read(text, &names))
    });
    let hits = Memo::new(move |_| parsed.with(|parsed| query.with(|text| Hits::of(parsed, text))));
    let saved = Memo::new(move |_| own.get().flatten().map(|own| (own.text, own.public)));
    let how = Memo::new(move |_| {
        saved.with(|saved| {
            let saved = saved
                .as_ref()
                .map(|(text, public)| (text.as_str(), *public));
            query.with(|text| listing(boxed.side.get(), saved, text))
        })
    });
    let asked = Memo::new(move |_| {
        saved.with(|saved| {
            let saved = saved
                .as_ref()
                .map(|(text, public)| (text.as_str(), *public));
            query.with(|text| public_query(boxed.side.get(), saved, text))
        })
    });
    let found = fetch_public(asked);
    let reader = room.clone();
    let project = Memo::new(move |_| match shown.get() {
        _ if how.get() == Listing::Picked && shown.get() == Route::AllEvents => Ok(Vec::new()),
        Route::Catalogue(uuid) => reader.read(|txn| {
            let previewed = match (how.get(), parsed.get()) {
                (Listing::Picked, _) | (Listing::Preview, Query::Empty) => Some(None),
                (Listing::Preview, Query::Words | Query::Rule(_)) => {
                    let names = cocat_core::model::catalogue_names(txn);
                    query
                        .with(|text| cocat_core::query::parse(text, &names))
                        .ok()
                        .map(Some)
                }
                _ => None,
            };
            match previewed {
                Some(search) => preview_rows(txn, &uuid, search.as_ref()),
                None => rows(txn, &uuid),
            }
            .map_err(|error| error.to_string())
        }),
        Route::Trash => Ok(reader.read(|txn| trash_rows(txn))),
        Route::AllEvents => reader
            .read(|txn| all_rows(txn))
            .map_err(|error| error.to_string()),
        _ => Ok(Vec::new()),
    });
    let members = Members::Live(Memo::new(move |_| {
        let project = project.get()?;
        match asked.get() {
            None => Ok(project),
            Some(_) => found.with(|found| with_public(project, found)),
        }
    }));
    let with_search = Signal::derive(move || match how.get() {
        Listing::Members => own.with(|own| matches!(own, Some(Some(_)))),
        Listing::Preview | Listing::Picked => query.with(|text| !text.trim().is_empty()),
    });
    let period = RwSignal::new(kept.period);
    let sorting = RwSignal::new((SortKey::Start, false));
    let reader = room.clone();
    let matching = Memo::new(move |_| {
        members.with(|members| {
            let Ok(rows) = members else {
                return Vec::new();
            };
            let everything = || (0..rows.len()).collect();
            if boxed.side.get().searches() {
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
    // Not on the first run: the selection the preview handed over stays.
    Effect::watch(
        move || shown.track(),
        move |_, _, _| {
            form.set(Form::Closed);
            picked.set(None);
            building.set(false);
            period.set(None);
            selected.set(None);
        },
        false,
    );
    // A page opens on the catalogue's own search; the filter's text stays when it is saved.
    Effect::new(move |_| {
        shown.track();
        boxed.open(own.get_untracked());
    });
    Effect::new(move |_| {
        let saved = own.get().flatten();
        if boxed.side.get_untracked().searches() {
            query.set(saved.map(|own| own.text).unwrap_or_default());
        }
    });
    let placeholder = Signal::derive(move || match (shown.get(), boxed.side.get()) {
        (Route::Catalogue(_), Side::Search) => "Search all events for this catalogue".to_owned(),
        (Route::Catalogue(_), Side::Public) => {
            "Search the public catalogues for this catalogue".to_owned()
        }
        (_, Side::Public) => "Search every public catalogue".to_owned(),
        (Route::Catalogue(_), Side::Filter) => {
            let held = members.with(|members| members.map_or(0, <[Row]>::len));
            format!("Filter these {held} events")
        }
        (Route::Trash, _) => "Filter the trash".to_owned(),
        _ => "Search all events".to_owned(),
    });
    let can_write = room.can_write();
    // As the Catalogue menu offers it: on a catalogue, to those who may write.
    let renamable = move || can_write && matches!(shown.get(), Route::Catalogue(_));
    let reader = room.clone();
    let about = live_about(&room, shown);
    let counts = live_counts(&room, shown, about, members, order, with_search);
    let empty = Signal::derive(move || {
        let filtered = members.with(|members| members.is_ok_and(|rows| !rows.is_empty()));
        empty_text(showing(&reader, &shown.get()), filtered, can_write)
    });
    let linker = room.clone();
    let empty_link = Signal::derive(move || {
        let filtered = members.with(|members| members.is_ok_and(|rows| !rows.is_empty()));
        offers_public(showing(&linker, &shown.get()), filtered, can_write).then(|| {
            let page = window().location().pathname().unwrap_or_default();
            (
                "Browse the public catalogues",
                Route::Public(None).href(&page),
            )
        })
    });
    let keys =
        Memo::new(move |_| members.with(|members| members.map(attribute_keys).unwrap_or_default()));
    view! {
        <div class="page-head">
            <Heading about counts members rename=(Signal::derive(renamable), Callback::new(move |()| form.set(Form::Rename)))/>
            <div class="tools">
                <Show when=move || matches!(shown.get(), Route::Catalogue(_) | Route::Trash | Route::AllEvents)>
                    <SearchBox query building placeholder own=Signal::derive(move || boxed.searching())>
                        <SearchIcon/>
                    </SearchBox>
                </Show>
                <ColumnPicker keys columns picked/>
                <Actions shown selected form proposals/>
                {public.then(|| view! { <PublicButtons shown form editor=can_write/> })}
            </div>
        </div>
        <SearchLine shown boxed parsed own/>
        {move || found.with(|found| found_note(asked.get().is_some(), found))}
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
            (Form::Rename, Route::Catalogue(uuid)) => Some(view! { <RenameForm uuid form/> }.into_any()),
            _ => None,
        }}
        {move || (shown.get() == Route::Trash).then(|| view! {
            <Hint anchor="catalogues" topic="the trash"
                text="Deleted events and catalogues wait here. Restore puts one back; Delete for good cannot be undone."/>
            <TrashedCatalogues/>
        })}
        <Show when=move || members.with(|members| members.is_ok_and(|rows| !rows.is_empty()))>
            <Timeline members matching period selected/>
        </Show>
        <PeriodLine period order members/>
        <Table members order sorting columns selected empty empty_link scroll_top hits/>
    }
}

/// What the server answered for the public catalogues' search: `None` while it searches.
type PublicFound = RwSignal<Option<Result<Found, String>>>;

/// Asks the server for each search `asked` names, once typing pauses; a late answer to an
/// older search is dropped.
fn fetch_public(asked: Memo<Option<String>>) -> PublicFound {
    let found: PublicFound = RwSignal::new(None);
    let api = expect_context::<Projects>().api();
    Effect::new(move |_| {
        let Some(text) = asked.get() else {
            found.set(None);
            return;
        };
        found.set(None);
        let api = api.clone();
        set_timeout(
            move || {
                if asked.get_untracked().as_deref() != Some(text.as_str()) {
                    return;
                }
                spawn_local(async move {
                    let answer = crate::api::public_events(&api, &text)
                        .await
                        .map_err(|error| error.to_string());
                    if asked.get_untracked().as_deref() == Some(text.as_str()) {
                        found.try_set(Some(answer));
                    }
                });
            },
            SEARCH_PAUSE,
        );
    });
    found
}

/// How long typing pauses before the server searches the public catalogues.
const SEARCH_PAUSE: std::time::Duration = std::time::Duration::from_millis(300);

/// The project's rows and the public catalogues' in time order; an error of the server's
/// search stands for both, as a rule's would.
fn with_public(
    mut rows: Vec<Row>,
    found: &Option<Result<Found, String>>,
) -> Result<Vec<Row>, String> {
    let Some(found) = found else {
        return Ok(rows);
    };
    let found = found.clone()?;
    let public = expect_context::<Projects>().public;
    rows.extend(
        public.with_untracked(|all| public_events::rows(found, all.as_deref().unwrap_or_default())),
    );
    rows.sort_by_key(|row| (row.start, row.stop));
    Ok(rows)
}

/// While the server searches, and when it found more than the page shows.
fn found_note(asked: bool, found: &Option<Result<Found, String>>) -> Option<AnyView> {
    match found {
        None if asked => Some(
            view! { <p class="quiet search-note" role="status">"Searching the public catalogues…"</p> }
                .into_any(),
        ),
        Some(Ok(found)) if found.indexing > 0 => {
            let plural = if found.indexing == 1 { " is" } else { "s are" };
            Some(
                view! {
                    <p class="quiet search-note" role="status">{format!(
                        "{} public catalogue{plural} still being indexed on the server and not searched yet: search again in a few minutes.",
                        found.indexing
                    )}</p>
                }
                .into_any(),
            )
        }
        Some(Ok(found)) if found.total > found.events.len() => Some(
            view! {
                <p class="quiet search-note">{format!(
                    "The public catalogues hold {} matching events: the first {} by start time are shown. Narrow the search to see the others.",
                    found.total, found.events.len()
                )}</p>
            }
            .into_any(),
        ),
        _ => None,
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
        Route::AllEvents => Shown::Catalogue { smart: false },
        _ if use_context::<InProposal>().is_some() => Shown::NoProposal,
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
            <details class="columns" data-menu>
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
pub fn SearchIcon() -> impl IntoView {
    view! {
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
            stroke-width="2" aria-hidden="true">
            <circle cx="11" cy="11" r="7"></circle>
            <path d="M20 20l-3.5-3.5"></path>
        </svg>
    }
}
