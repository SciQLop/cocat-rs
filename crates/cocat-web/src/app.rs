//! The page's views. Logic the views need lives in the native-tested modules beside this one.

use leptos::prelude::*;

use crate::connection;
use crate::route::Route;
use crate::session::{ConfigError, RoomConfig};
use crate::state::Room;
use crate::views::header::Header;
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
    let route = RwSignal::new(current_route());
    // The page lives as long as the listener: nothing to remove it for.
    let _ = window_event_listener(leptos::ev::hashchange, move |_| route.set(current_route()));
    let shown = Memo::new(move |_| {
        let first = room.read(|txn| model::catalogues(txn).into_iter().next().map(|c| c.uuid));
        route.get().or_first(first.as_deref())
    });
    view! {
        <Header/>
        <div class="browser">
            <Sidebar shown/>
            <main class="content">
                <Shown shown/>
            </main>
        </div>
    }
    .into_any()
}

fn current_route() -> Route {
    Route::parse(&location_hash().unwrap_or_default())
}

#[component]
fn Shown(shown: Memo<Route>) -> impl IntoView {
    let room = expect_context::<Room>();
    let title = move || match shown.get() {
        Route::Catalogue(uuid) => room
            .read(|txn| model::catalogue(txn, &uuid))
            .map(|catalogue| catalogue.name)
            .unwrap_or_else(|| "No such catalogue".to_owned()),
        Route::Trash => "Trash".to_owned(),
        Route::Default => "No catalogue yet".to_owned(),
    };
    view! { <h1>{title}</h1> }
}
