//! The page's views. Logic the views need lives in the native-tested modules beside this one.

use leptos::prelude::*;

use crate::connection;
use crate::route::Route;
use crate::rows::Row;
use crate::session::{ConfigError, RoomConfig};
use crate::state::Room;
use crate::views::catalogue::CataloguePage;
use crate::views::event_panel::{EventPanel, Shown};
use crate::views::groups::GroupsPage;
use crate::views::header::Header;
use crate::views::history::HistoryPage;
use crate::views::notice::{Notice, NoticeBar};
use crate::views::projects::Projects;
use crate::views::sidebar::Sidebar;
use cocat_core::model;

pub fn start() {
    console_error_panic_hook::set_once();
    match read_config() {
        Ok(config) => leptos::mount::mount_to_body(move || view! { <Shell config/> }),
        Err(error) => leptos::mount::mount_to_body(move || failure(error.to_string())),
    }
}

fn read_config() -> Result<RoomConfig, ConfigError> {
    let base = document().base_uri().ok().flatten().unwrap_or_default();
    RoomConfig::from_meta(meta, &base)
}

fn meta(name: &str) -> Option<String> {
    document()
        .query_selector(&format!("meta[name=\"{name}\"]"))
        .ok()
        .flatten()?
        .get_attribute("content")
}

/// What the server's last action said, when a form of the old pages sent the visitor here.
fn read_notice() -> Option<Notice> {
    Some(Notice {
        text: meta("cocat-flash")?,
        link: meta("cocat-share-link"),
    })
}

fn failure(message: String) -> impl IntoView {
    view! {
        <main class="loading">
            <p role="alert">{message}</p>
        </main>
    }
}

/// Holds the open project; each project gets an `App` of its own, so switching starts from a
/// fresh room, connection and view state.
#[component]
fn Shell(config: RoomConfig) -> impl IntoView {
    let projects = Projects::new(config, read_notice());
    provide_context(projects);
    projects.reload();
    // The shell lives as long as the page: nothing to remove the listener for.
    let _ = window_event_listener(leptos::ev::popstate, move |_| projects.arrived());
    let opened = Memo::new(move |_| projects.open.with(|config| config.room_id.clone()));
    move || {
        opened.with(|_| {
            let config = projects.open.get_untracked();
            view! { <App config/> }
        })
    }
}

#[component]
fn App(config: RoomConfig) -> impl IntoView {
    let room = Room::new(config);
    provide_context(room.clone());
    let connection = match connection::connect(room.clone()) {
        Ok(connection) => StoredValue::new_local(connection),
        Err(error) => return failure(format!("The project cannot sync: {error}")).into_any(),
    };
    on_cleanup(move || {
        connection.try_with_value(|connection| connection.close());
    });
    let (editor, published) = (room.can_write(), room.editing);
    let route = RwSignal::new(current_route());
    let listener =
        window_event_listener(leptos::ev::hashchange, move |_| route.set(current_route()));
    on_cleanup(move || listener.remove());
    let shown = Memo::new(move |_| {
        let first = room.read(|txn| model::catalogues(txn).into_iter().next().map(|c| c.uuid));
        route.get().or_first(first.as_deref())
    });
    let selected = RwSignal::new(None::<Row>);
    let screen = RwSignal::new(None);
    let panel = Memo::new(move |_| selected.with(Shown::of));
    Effect::new(move |_| {
        let editing = panel.with(|shown| shown.event().map(str::to_owned));
        published.set(editing.filter(|_| editor));
    });
    let in_history = Memo::new(move |_| matches!(shown.get(), Route::History { .. }));
    let in_groups = Memo::new(move |_| matches!(shown.get(), Route::Groups | Route::Group(_)));
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
        if in_history.get() || in_groups.get() {
            selected.set(None);
        }
    });
    view! {
        <Header shown/>
        <NoticeBar/>
        {move || if in_history.get() {
            view! { <HistoryPage scope picked/> }.into_any()
        } else if in_groups.get() {
            view! { <GroupsPage shown/> }.into_any()
        } else {
            view! {
                <div class="browser">
                    <Sidebar shown/>
                    <main class="content">
                        <CataloguePage shown selected screen/>
                    </main>
                    <EventPanel selected shown=panel route=shown screen/>
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
