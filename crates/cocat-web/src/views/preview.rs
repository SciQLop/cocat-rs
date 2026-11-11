//! The open catalogue while the project downloads: its events a page at a time from the
//! server, read-only, under the live page's heading and tools so the switch does not move
//! the table.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::period::Period;
use crate::preview::{FIRST_PAGE, NEXT_PAGE, PreviewState, keeps_preview, wanted};
use crate::route::Route;
use crate::rows::{Row, SortKey, attribute_keys};
use crate::search::{self, Side};
use crate::session::RoomConfig;
use crate::state::Room;
use crate::submissions::public_actions;
use crate::views::catalogue::{Members, SearchIcon};
use crate::views::public::relay_of;
use crate::views::rule::{About, Heading, catalogue_about};
use crate::views::search::{Boxed, SearchBox, SearchLine, own_of};
use crate::views::table::Table;
use crate::views::timeline::{PeriodLine, Timeline};
use cocat_core::room_id::is_public_id;

/// Where the visitor is in the preview: the table's scroll, the selection and the period.
pub type Place = (
    RwSignal<f64>,
    RwSignal<Option<Row>>,
    RwSignal<Option<Period>>,
);

/// Reads the pages of the catalogue `shown` into `state` while the project downloads. A route
/// to another catalogue starts over from the top with nothing selected; the project's arrival
/// stops the reading and drops the pages.
pub fn load_preview(
    room: &Room,
    shown: Memo<Route>,
    state: RwSignal<Option<PreviewState>>,
    place: Place,
) {
    let (config, loading) = (room.config.clone(), room.loading);
    let reading = Rc::new(Cell::new(0_u64));
    Effect::new(move |_| {
        let route = shown.get();
        if loading.with_untracked(Option::is_none) {
            return;
        }
        let asked = match &route {
            Route::Catalogue(uuid) => Some(uuid.clone()),
            _ => None,
        };
        let previewed =
            state.with_untracked(|state| state.as_ref().map(|state| state.catalogue.uuid.clone()));
        if keeps_preview(asked.as_deref(), previewed.as_deref()) {
            return;
        }
        let (scroll_top, selected, period) = place;
        state.set(None);
        scroll_top.set(0.0);
        selected.set(None);
        period.set(None);
        let this = reading.get() + 1;
        reading.set(this);
        if !matches!(route, Route::Catalogue(_) | Route::Default) {
            return;
        }
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
    config: Arc<RoomConfig>,
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
    let Some(catalogue) = wanted(asked.as_deref(), &listed).cloned() else {
        return;
    };
    if !wanted_still() {
        return;
    }
    let uuid = catalogue.uuid.clone();
    let mut next = first.next.clone();
    state.try_set(Some(PreviewState::first(&listed, catalogue, first)));
    while let Some(cursor) = next.take() {
        let answer = api::catalogue_events(&config, &uuid, NEXT_PAGE, Some(&cursor)).await;
        if !wanted_still() {
            return;
        }
        state.try_update(|state| match (state, answer) {
            (Some(state), Ok(page)) => {
                next = page.next.clone();
                state.append(&cursor, page);
            }
            (Some(state), Err(_)) => state.stall(),
            (None, _) => {}
        });
    }
}

#[component]
pub fn PreviewView(
    state: RwSignal<Option<PreviewState>>,
    selected: RwSignal<Option<Row>>,
    scroll_top: RwSignal<f64>,
    period: RwSignal<Option<Period>>,
) -> impl IntoView {
    let loading = expect_context::<Room>().loading;
    let members = Members::Preview(state);
    let all = Memo::new(move |_| {
        members.with(|rows| (0..rows.map_or(0, <[Row]>::len)).collect::<Vec<_>>())
    });
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
    let about = Memo::new(move |_| {
        state.with(|state| {
            let state = state.as_ref()?;
            let name = |uuid: &str| {
                let (_, name) = state.names.iter().find(|(id, _)| id == uuid)?;
                Some(name.clone())
            };
            Some(catalogue_about(
                state.catalogue.clone(),
                &state.names,
                &name,
            ))
        })
    });
    let counts = Signal::derive(move || {
        let progress = loading
            .get()
            .map(|loading| format!(" · {}", loading.text()));
        state
            .with(|state| state.as_ref().map(PreviewState::header))
            .map(|header| format!("{header}{}", progress.unwrap_or_default()))
    });
    let own = Memo::new(move |_| {
        state.with(|state| {
            state
                .as_ref()
                .and_then(|state| own_of(state.catalogue.rule.as_ref(), &state.names))
        })
    });
    let name = Signal::derive(move || {
        state.with(|state| {
            state
                .as_ref()
                .map(|state| state.catalogue.name.clone())
                .unwrap_or_default()
        })
    });
    let boxed = Boxed::new();
    Effect::new(move |_| boxed.open(own.get()));
    let parsed = Memo::new(move |_| {
        let names = state.with(|state| state.as_ref().map(|state| state.names.clone()));
        boxed
            .query
            .with(|text| search::read(text, &names.unwrap_or_default()))
    });
    let columns = Memo::new(move |_| {
        state.with(|state| {
            state
                .as_ref()
                .map(|state| state.catalogue.columns.clone())
                .unwrap_or_default()
        })
    });
    let keys =
        Memo::new(move |_| members.with(|rows| rows.map(attribute_keys).unwrap_or_default()));
    let shown = Memo::new(move |_| {
        state.with(|state| {
            state.as_ref().map_or(Route::Default, |state| {
                Route::Catalogue(state.catalogue.uuid.clone())
            })
        })
    });
    let placeholder = Signal::derive(move || match boxed.side.get() {
        Side::Search => "Search all events for this catalogue".to_owned(),
        Side::Filter => "Filter once the project has loaded".to_owned(),
    });
    let building = RwSignal::new(false);
    let about = Memo::new(move |_| about.get().unwrap_or_else(|| About::titled("")));
    view! {
        <div class="page-head">
            <Heading about counts members/>
            <div class="tools">
                <SearchBox query=boxed.query building placeholder frozen=true
                    own=Signal::derive(move || boxed.searching())>
                    <SearchIcon/>
                </SearchBox>
                <PreviewButtons keys/>
            </div>
        </div>
        <SearchLine shown boxed parsed own frozen=name/>
        <Timeline members matching=all period selected/>
        <PeriodLine period order members/>
        <Table members order sorting columns selected empty=Signal::stored("") scroll_top sortable=false/>
    }
}

/// The live page's buttons, disabled until the project has loaded, so the tools take the
/// same room.
#[component]
fn PreviewButtons(keys: Memo<Vec<String>>) -> impl IntoView {
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
        <Show when=move || keys.with(|keys| !keys.is_empty())>
            <button type="button" class="button" disabled>"Columns"</button>
        </Show>
        <button type="button" class="button" disabled>"More"</button>
        {editor.then(|| view! { <button type="button" class="button primary" disabled>"New event"</button> })}
        {public_buttons}
    }
}
