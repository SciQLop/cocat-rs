//! The page's views. Logic the views need lives in the native-tested modules beside this one.

use leptos::prelude::*;

use crate::connection;
use crate::route::Route;
use crate::rows::Row;
use crate::session::{ConfigError, RoomConfig};
use crate::state::Room;
use crate::views::catalogue::CataloguePage;
use crate::views::event_panel::{EventPanel, Shown};
use crate::views::files::{FilePane, FilesLoader, FilesState, ProjectFiles};
use crate::views::groups::GroupsPage;
use crate::views::header::Header;
use crate::views::history::HistoryPage;
use crate::views::home::HomeSpace;
use crate::views::links::{LinkedCatalogue, Lookups};
use crate::views::notice::{Notice, NoticeBar};
use crate::views::projects::Projects;
use crate::views::projects::set_title;
use crate::views::public::{PublicSidebar, PublicSpace};
use crate::views::rounds::RoundsPage;
use crate::views::sidebar::Sidebar;
use crate::views::transfer::{ImportDialog, Importer, drop_on};
use crate::views::upload::Uploads;
use cocat_core::model;

pub fn start() {
    console_error_panic_hook::set_once();
    crate::views::menus::close_menus_from_outside();
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
/// fresh room, connection and view state. The public catalogues' index holds no project: the
/// last one's `App` goes, and its connection with it.
#[component]
fn Shell(config: RoomConfig) -> impl IntoView {
    let projects = Projects::new(config, read_notice());
    provide_context(projects);
    projects.reload();
    // The shell lives as long as the page: nothing to remove the listeners for.
    let _ = window_event_listener(leptos::ev::popstate, move |_| projects.arrived());
    let _ = window_event_listener(leptos::ev::hashchange, move |_| projects.follow_hash());
    let on_index = Memo::new(move |_| projects.route.with(Route::is_public_index));
    let at_home = Memo::new(move |_| projects.route.with(|route| *route == Route::Home));
    let opened = Memo::new(move |_| projects.open.with(|config| config.room_id.clone()));
    move || {
        if at_home.get() {
            return view! { <HomeSpace/> }.into_any();
        }
        if on_index.get() {
            return view! { <PublicSpace/> }.into_any();
        }
        opened.with(|_| {
            let config = projects.open.get_untracked();
            view! { <App config/> }.into_any()
        })
    }
}

#[component]
fn App(config: RoomConfig) -> impl IntoView {
    set_title(&config.room_name);
    let public = cocat_core::room_id::is_public_id(&config.room_id);
    let current = public.then(|| config.room_id.clone());
    let config_has_files = config.files;
    let loader_config = config.clone();
    let room = Room::new(config);
    provide_context(room.clone());
    Lookups::provide(&room);
    let files: ProjectFiles = RwSignal::new(FilesState::initial(config_has_files));
    provide_context(files);
    // An upload into a project without files makes its file list; the page syncs it then.
    let files_on = Uploads::provide(config_has_files).files_on;
    let connection = match connection::connect_project(room.clone()) {
        Ok(connection) => StoredValue::new_local(connection),
        Err(error) => return failure(format!("The project cannot sync: {error}")).into_any(),
    };
    on_cleanup(move || {
        connection.try_with_value(|connection| connection.close());
    });
    let (editor, published) = (room.can_write(), room.editing);
    let route = expect_context::<Projects>().route;
    let shown = Memo::new(move |_| {
        let first = room.read(|txn| model::catalogues(txn).into_iter().next().map(|c| c.uuid));
        route.get().or_first(first.as_deref())
    });
    let selected = RwSignal::new(None::<Row>);
    let screen = RwSignal::new(None);
    let importer = Importer::new();
    provide_context(importer);
    let (drag_over, drop) = drop_on(importer, editor);
    let panel = Memo::new(move |_| selected.with(Shown::of));
    Effect::new(move |_| {
        let editing = panel.with(|shown| shown.event().map(str::to_owned));
        published.set(editing.filter(|_| editor));
    });
    let file_path = Memo::new(move |_| match shown.get() {
        Route::File(path) => Some(path),
        _ => None,
    });
    let link = Memo::new(move |_| match shown.get() {
        Route::Link(id) => Some(id),
        _ => None,
    });
    let in_history = Memo::new(move |_| matches!(shown.get(), Route::History { .. }));
    let in_groups = Memo::new(move |_| matches!(shown.get(), Route::Groups | Route::Group(_)));
    let in_rounds = Memo::new(move |_| {
        matches!(
            shown.get(),
            Route::Rounds | Route::NewRound | Route::Round(_)
        )
    });
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
        let elsewhere = file_path.with(Option::is_some) || link.with(Option::is_some);
        if in_history.get() || in_groups.get() || in_rounds.get() || elsewhere {
            selected.set(None);
        }
    });
    view! {
        {move || files_on.get().then(|| view! { <FilesLoader config=loader_config.clone() state=files/> })}
        <Header shown/>
        <NoticeBar/>
        {move || importer.is_open().then(|| view! { <ImportDialog/> })}
        {move || if in_history.get() {
            view! { <HistoryPage scope picked/> }.into_any()
        } else if in_groups.get() {
            view! { <GroupsPage shown/> }.into_any()
        } else if in_rounds.get() {
            view! { <RoundsPage shown/> }.into_any()
        } else {
            view! {
                <div class="browser">
                    {match current.clone() {
                        Some(current) => view! { <PublicSidebar current=Some(current)/> }.into_any(),
                        None => view! { <Sidebar shown/> }.into_any(),
                    }}
                    <div class="pane">
                        {move || match (file_path.get(), link.get()) {
                            (_, Some(id)) => view! { <LinkedCatalogue id/> }.into_any(),
                            (Some(path), None) => view! {
                                <main class="content file-pane" on:dragover=drag_over on:drop=drop><FilePane state=files path/></main>
                            }.into_any(),
                            (None, None) => view! {
                                <main class="content" on:dragover=drag_over on:drop=drop>
                                    <CataloguePage shown selected screen/>
                                </main>
                                <EventPanel selected shown=panel route=shown screen/>
                            }.into_any(),
                        }}
                    </div>
                </div>
            }
            .into_any()
        }}
    }
    .into_any()
}
