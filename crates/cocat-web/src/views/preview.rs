//! The open catalogue while the project downloads: its events a page at a time from the
//! server, read-only, laid out like the live page so the switch does not move the table.

use std::cell::Cell;
use std::rc::Rc;

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::period::Period;
use crate::preview::{FIRST_PAGE, NEXT_PAGE, PreviewState, wanted};
use crate::route::Route;
use crate::rows::{Row, SortKey};
use crate::state::Room;
use crate::submissions::public_actions;
use crate::views::catalogue::{Members, SearchIcon};
use crate::views::public::{relay_line, relay_of};
use crate::views::table::Table;
use crate::views::timeline::{PeriodLine, Timeline};
use cocat_core::room_id::is_public_id;

/// Reads the pages of the catalogue `shown` into `state` while the project downloads; a new
/// route starts over, and the project's arrival stops the reading and drops the pages.
pub fn load_preview(room: &Room, shown: Memo<Route>, state: RwSignal<Option<PreviewState>>) {
    let (config, loading) = (room.config.clone(), room.loading);
    let reading = Rc::new(Cell::new(0_u64));
    Effect::new(move |_| {
        let asked = match shown.get() {
            Route::Catalogue(uuid) => Some(uuid),
            Route::Default => None,
            _ => {
                state.set(None);
                return;
            }
        };
        state.set(None);
        if loading.with_untracked(Option::is_none) {
            return;
        }
        let this = reading.get() + 1;
        reading.set(this);
        let current = reading.clone();
        let wanted_still = move || {
            current.get() == this
                && !state.is_disposed()
                && loading.try_with_untracked(Option::is_some) == Some(true)
        };
        spawn_local(read_pages(config.clone(), asked, state, wanted_still));
    });
    Effect::new(move |_| {
        if loading.with(Option::is_none) {
            state.set(None);
        }
    });
}

// simplify: each page makes the server read the whole catalogue again (about a dozen times
// for STIX); a per-room sorted cache on the server is the upgrade if that ever shows.
async fn read_pages(
    config: std::sync::Arc<crate::session::RoomConfig>,
    asked: Option<String>,
    state: RwSignal<Option<PreviewState>>,
    wanted_still: impl Fn() -> bool,
) {
    let (listed, first) = match &asked {
        Some(uuid) => futures_util::join!(
            api::catalogues(&config),
            api::catalogue_events(&config, uuid, FIRST_PAGE, None),
        ),
        None => {
            let listed = api::catalogues(&config).await;
            let first = match listed.as_ref().ok().and_then(|listed| wanted(None, listed)) {
                Some(about) => api::catalogue_events(&config, &about.uuid, FIRST_PAGE, None).await,
                None => return,
            };
            (listed, first)
        }
    };
    let (Ok(listed), Ok(first)) = (listed, first) else {
        return;
    };
    let Some(about) = wanted(asked.as_deref(), &listed).cloned() else {
        return;
    };
    if !wanted_still() {
        return;
    }
    let uuid = about.uuid.clone();
    let mut next = first.next.clone();
    state.try_set(Some(PreviewState::first(about, first)));
    while let Some(cursor) = next.take() {
        let Ok(page) = api::catalogue_events(&config, &uuid, NEXT_PAGE, Some(&cursor)).await else {
            return;
        };
        if !wanted_still() {
            return;
        }
        next = page.next.clone();
        state.try_update(|state| {
            if let Some(state) = state {
                state.append(&cursor, page);
            }
        });
    }
}

#[component]
pub fn PreviewView(
    state: RwSignal<Option<PreviewState>>,
    selected: RwSignal<Option<Row>>,
    scroll_top: RwSignal<f64>,
) -> impl IntoView {
    let loading = expect_context::<Room>().loading;
    let members = Members::Preview(state);
    let all = Memo::new(move |_| {
        members.with(|rows| (0..rows.map_or(0, <[Row]>::len)).collect::<Vec<_>>())
    });
    let period = RwSignal::new(None::<Period>);
    let order = Memo::new(move |_| {
        members.with(|rows| {
            let rows = rows.unwrap_or_default();
            let mut order = all.get();
            if let Some(period) = period.get() {
                order.retain(|&index| period.overlaps(rows[index].start, rows[index].stop));
            }
            order
        })
    });
    let sorting = RwSignal::new((SortKey::Start, false));
    let title = move || state.with(|state| state.as_ref().map(|state| state.about.name.clone()));
    let header = move || state.with(|state| state.as_ref().map(PreviewState::header));
    let columns = Memo::new(move |_| {
        state.with(|state| {
            state
                .as_ref()
                .map(|state| state.about.columns.clone())
                .unwrap_or_default()
        })
    });
    let progress = move || {
        loading
            .get()
            .map(|loading| format!(" · {}", loading.text()))
    };
    view! {
        <div class="page-head">
            <div class="heading">
                <h1>{title}</h1>
                <p class="quiet" role="status">{header}{progress}</p>
                <RelayLine/>
            </div>
            <PreviewTools/>
        </div>
        <div class="search-line">
            <div class="scope" role="group" aria-label="Search in">
                <span class="quiet">"Search in"</span>
                <button type="button" class="scope-option" aria-pressed="true" disabled>"This catalogue"</button>
                <button type="button" class="scope-option" aria-pressed="false" disabled>"All events"</button>
            </div>
            <span class="quiet">"Search, sorting and editing open once the project has loaded."</span>
        </div>
        <Timeline members matching=all period selected/>
        <PeriodLine period order members/>
        <Table members order sorting columns selected empty=Signal::stored("") scroll_top sortable=false/>
    }
}

/// Where a public catalogue's events come from, as the live heading says it.
#[component]
fn RelayLine() -> impl IntoView {
    let id = expect_context::<Room>().config.room_id.clone();
    let relay = is_public_id(&id).then(|| relay_of(id));
    move || {
        relay
            .and_then(|relay| relay.get())
            .map(|relay| view! { <p class="quiet">{relay_line(&relay)}</p> })
    }
}

/// The live page's tools, disabled until the project has loaded, so the table below stays
/// where the live page puts it.
#[component]
fn PreviewTools() -> impl IntoView {
    let room = expect_context::<Room>();
    let editor = room.can_write();
    let public = is_public_id(&room.config.room_id);
    let relayed = public.then(|| relay_of(room.config.room_id.clone()));
    let public_buttons = move || {
        let relayed = relayed.is_some_and(|relay| relay.with(Option::is_some));
        public_actions(public, editor, relayed)
            .iter()
            .map(|&(action, main)| {
                view! { <button type="button" class="button" class:primary=main disabled>{action.label()}</button> }
            })
            .collect_view()
    };
    view! {
        <div class="tools">
            <label class="search">
                <SearchIcon/>
                <span class="search-field">
                    <input type="text" disabled aria-label="Search events"
                        placeholder="Filter once the project has loaded"/>
                </span>
            </label>
            <button type="button" class="button" disabled>"Columns"</button>
            <button type="button" class="button" disabled>"More"</button>
            {editor.then(|| view! { <button type="button" class="button primary" disabled>"New event"</button> })}
            {public_buttons}
        </div>
    }
}
