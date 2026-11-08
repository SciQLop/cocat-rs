//! The page's views. Logic the views need lives in the native-tested modules beside this one.

use leptos::prelude::*;

use crate::connection;
use crate::route::Route;
use crate::rows::Row;
use crate::session::{ConfigError, RoomConfig};
use crate::state::Room;
use crate::views::catalogue::CataloguePage;
use crate::views::event_panel::{EventPanel, Shown};
use crate::views::header::Header;
use crate::views::history::HistoryPage;
use crate::views::sidebar::Sidebar;
use cocat_core::model;

pub fn start() {
    console_error_panic_hook::set_once();
    match read_config() {
        Ok(config) => leptos::mount::mount_to_body(move || view! { <App config/> }),
        Err(error) => leptos::mount::mount_to_body(move || failure(error.to_string())),
    }
}

fn read_config() -> Result<RoomConfig, ConfigError> {
    let document = document();
    let base = document.base_uri().ok().flatten().unwrap_or_default();
    let meta = |name: &str| {
        document
            .query_selector(&format!("meta[name=\"{name}\"]"))
            .ok()
            .flatten()?
            .get_attribute("content")
    };
    RoomConfig::from_meta(meta, &base)
}

fn failure(message: String) -> impl IntoView {
    view! {
        <main class="loading">
            <p role="alert">{message}</p>
        </main>
    }
}

#[component]
fn App(config: RoomConfig) -> impl IntoView {
    let room = Room::new(config);
    provide_context(room.clone());
    if let Err(error) = connection::connect(room.clone()) {
        return failure(format!("The room cannot sync: {error}")).into_any();
    }
    let (editor, published) = (room.can_write(), room.editing);
    let route = RwSignal::new(current_route());
    // The page lives as long as the listener: nothing to remove it for.
    let _ = window_event_listener(leptos::ev::hashchange, move |_| route.set(current_route()));
    let shown = Memo::new(move |_| {
        let first = room.read(|txn| model::catalogues(txn).into_iter().next().map(|c| c.uuid));
        route.get().or_first(first.as_deref())
    });
    let selected = RwSignal::new(None::<Row>);
    let panel = Memo::new(move |_| selected.with(Shown::of));
    Effect::new(move |_| {
        let editing = panel.with(|shown| shown.event().map(str::to_owned));
        published.set(editing.filter(|_| editor));
    });
    let in_history = Memo::new(move |_| matches!(shown.get(), Route::History { .. }));
    // Both keep their value once the page leaves the history: see `HistoryPage`.
    let scope = Memo::new(move |kept: Option<&Option<String>>| match shown.get() {
        Route::History { catalogue, .. } => catalogue,
        _ => kept.cloned().flatten(),
    });
    let picked = Memo::new(move |kept: Option<&Option<u64>>| match shown.get() {
        Route::History { version, .. } => version,
        _ => kept.copied().flatten(),
    });
    Effect::new(move |_| {
        if in_history.get() {
            selected.set(None);
        }
    });
    view! {
        <Header shown/>
        {move || if in_history.get() {
            view! { <HistoryPage scope picked/> }.into_any()
        } else {
            view! {
                <div class="browser">
                    <Sidebar shown/>
                    <main class="content">
                        <CataloguePage shown selected/>
                    </main>
                    <EventPanel selected shown=panel route=shown/>
                </div>
            }
            .into_any()
        }}
    }
    .into_any()
}

fn current_route() -> Route {
    Route::parse(&location_hash().unwrap_or_default())
}
