//! The project menu in the header, and switching the page to another project.

use leptos::html::Details;
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsValue;

use crate::api;
use crate::history::browse_href;
use crate::projects::{Actions, Arrival, Project, arrival, stays, title};
use crate::public::PublicCatalogue;
use crate::route::Route;
use crate::session::RoomConfig;
use crate::state::Room;
use crate::views::controls::{Hint, go_to};
use crate::views::notice::Notice;
use crate::views::project_dialogs::{Dialog, ProjectDialog};
use crate::views::sidebar::PublicIcon;
use crate::views::transfer::Importer;
use cocat_core::room_id::is_public_id;

/// The visitor's projects and the one the page holds. The page shows the open one; changing
/// its id brings up a fresh room, connection and views. On the public catalogues' index the
/// page holds no project at all. The notice is about the project the page opened with, so it
/// goes when another one opens.
#[derive(Clone, Copy)]
pub struct Projects {
    pub list: RwSignal<Vec<Project>>,
    pub open: RwSignal<RoomConfig>,
    pub notice: RwSignal<Option<Notice>>,
    /// What the URL's hash names, for the whole page.
    pub route: RwSignal<Route>,
    /// The public catalogues, once read.
    pub public: RwSignal<Option<Vec<PublicCatalogue>>>,
}

impl Projects {
    pub fn new(config: RoomConfig, notice: Option<Notice>) -> Self {
        Self {
            list: RwSignal::new(Vec::new()),
            open: RwSignal::new(config),
            notice: RwSignal::new(notice),
            route: RwSignal::new(current_route()),
            public: RwSignal::new(None),
        }
    }

    /// Reads the public catalogues again; the error goes to `failed`.
    pub fn reload_public(self, failed: impl FnOnce(String) + 'static) {
        let api = self.api();
        spawn_local(async move {
            match api::read::<Vec<PublicCatalogue>>(&api, &["public"]).await {
                Ok(listed) => {
                    self.public.try_set(Some(listed));
                }
                Err(error) => failed(error.to_string()),
            }
        });
    }

    /// The page shows the public catalogues: their index, the submissions, or one of them.
    pub fn in_public(self) -> bool {
        let open = self.open.with(|config| config.room_id.clone());
        self.route
            .with(|route| route.in_public_catalogues(is_public_id(&open)))
    }

    /// The hash changed.
    pub fn follow_hash(self) {
        let now = current_route();
        if self.route.with_untracked(|route| *route != now) {
            self.route.set(now);
        }
    }

    pub fn reload(self) {
        let api = self.api();
        spawn_local(async move {
            if let Ok(listed) = api::projects(&api).await {
                self.list.try_set(listed);
            }
        });
    }

    pub fn api(self) -> String {
        self.open.with_untracked(|config| config.api_url.clone())
    }

    /// Opens `project` as a new step of the browser's history.
    pub fn switch(self, project: &Project) {
        self.switch_to(project, None);
    }

    /// Opens `project` on `view` (its default view when none) as a new step of the history.
    pub fn switch_to(self, project: &Project, view: Option<&Route>) {
        let open = self.open.with_untracked(|config| config.room_id.clone());
        let elsewhere = self.route.with_untracked(Route::holds_no_project);
        if stays(&open, &project.id, elsewhere) {
            return;
        }
        let Some(next) = self
            .open
            .with_untracked(|config| config.for_project(project))
        else {
            return;
        };
        let target = match view {
            Some(view) => view.href(&browse_href(&project.id)),
            None => browse_href(&project.id),
        };
        if let Ok(history) = window().history() {
            let _ = history.push_state_with_url(&JsValue::NULL, "", Some(&target));
        }
        if open != project.id {
            self.show(next);
        }
        self.follow_hash();
    }

    /// The browser went back or forward.
    pub fn arrived(self) {
        let path = window().location().pathname().unwrap_or_default();
        let open = self.open.with_untracked(|config| config.room_id.clone());
        let mut known = self.list.get_untracked();
        self.public.with_untracked(|public| {
            known.extend(public.iter().flatten().map(PublicCatalogue::project));
        });
        match arrival(&open, &path, &known) {
            Arrival::Stay => {}
            Arrival::Open(project) => {
                if let Some(next) = self
                    .open
                    .with_untracked(|config| config.for_project(&project))
                {
                    self.show(next);
                }
            }
            Arrival::Reload => {
                let _ = window().location().reload();
            }
        }
        self.follow_hash();
    }

    /// A project was made: it joins the list and opens.
    pub fn created(self, project: Project) {
        self.list.update(|listed| {
            listed.push(project.clone());
            listed.sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
        });
        self.switch(&project);
    }

    pub fn renamed(self, project: Project) {
        self.list.update(|listed| {
            if let Some(entry) = listed.iter_mut().find(|entry| entry.id == project.id) {
                entry.name = project.name.clone();
            }
        });
        if self
            .open
            .with_untracked(|config| config.room_id == project.id)
        {
            set_title(&project.name);
            self.open.update(|config| config.room_name = project.name);
        }
    }

    fn show(self, next: RoomConfig) {
        self.notice.set(None);
        self.open.set(next);
    }
}

pub fn set_title(name: &str) {
    document().set_title(&title(name));
}

fn current_route() -> Route {
    Route::parse(&window().location().hash().unwrap_or_default())
}

/// The open project's name, or "Public catalogues" on their pages, as a button that lists
/// where to go: the public catalogues and the visitor's projects. What can be done with the
/// open project is in [`ProjectActions`] beside it.
#[component]
pub fn ProjectMenu() -> impl IntoView {
    let projects = expect_context::<Projects>();
    let menu = NodeRef::<Details>::new();
    let dialog = RwSignal::new(None::<Dialog>);
    let open_id = move || projects.open.with(|config| config.room_id.clone());
    let public = move || projects.in_public();
    let at_home = move || projects.route.with(|route| *route == Route::Home);
    let label = move || match (at_home(), public()) {
        (true, _) => "Projects".to_owned(),
        (false, true) => "Public catalogues".to_owned(),
        (false, false) => projects.open.with(|config| config.room_name.clone()),
    };
    let close = move || close_menu(menu);
    let toggled = move |_| {
        if menu.get_untracked().is_some_and(|menu| menu.open()) {
            projects.reload();
        }
    };
    let entry = move |project: Project| {
        let id = project.id.clone();
        let here = move || !public() && !at_home() && id == open_id();
        let role = project.role.as_str();
        let target = project.clone();
        view! {
            <button type="button" role="menuitem" class="project-entry"
                aria-current=move || here().then_some("true")
                on:click=move |_| { close(); projects.switch(&target); }>
                <span class="label">{project.name}</span>
                <small>{role}</small>
            </button>
        }
    };
    view! {
        <details class="project-menu" data-menu node_ref=menu on:toggle=toggled
            on:keydown=move |event| if event.key() == "Escape" { close(); }>
            <summary class="project-switch" aria-label=move || format!("{}, switch project", label())>
                <Show when=public><PublicIcon size=16/></Show>
                <span class="label">{label}</span>
                <span class="caret" aria-hidden="true">"▾"</span>
            </summary>
            <div class="menu" role="menu" aria-label="Projects">
                <button type="button" role="menuitem" class="project-entry public-entry"
                    aria-current=move || public().then_some("true")
                    on:click=move |_| { close(); go_to(&Route::Public(None).hash()); }>
                    <span class="label">"Public catalogues"</span>
                    <small>"everyone"</small>
                </button>
                <hr/>
                <p class="eyebrow">"Projects"</p>
                <For each=move || projects.list.get() key=|project| (project.id.clone(), project.name.clone(), project.role) children=entry/>
                <hr/>
                <button type="button" role="menuitem" on:click=move |_| { close(); dialog.set(Some(Dialog::Create)); }>"+ New project"</button>
                <Hint text="A project is a set of catalogues shared with the same people." anchor="projects" topic="projects"/>
            </div>
        </details>
        {move || dialog.get().map(|which| view! { <ProjectDialog which dialog/> })}
    }
}

/// What can be done with the open project, beside its name: the everyday actions first, then
/// leaving or deleting it, set apart at the end. Nothing on the public catalogues or at home.
#[component]
pub fn ProjectActions() -> impl IntoView {
    let projects = expect_context::<Projects>();
    let Some(room) = use_context::<Room>() else {
        return ().into_any();
    };
    let editor = room.can_write();
    let importer = use_context::<Importer>();
    let menu = NodeRef::<Details>::new();
    let dialog = RwSignal::new(None::<Dialog>);
    let name = move || projects.open.with(|config| config.room_name.clone());
    let elsewhere =
        move || projects.in_public() || projects.route.with(|route| *route == Route::Home);
    let actions = move || {
        projects
            .open
            .with(|config| Actions::on(&config.room_id, config.role))
    };
    let pick = move |which: Dialog| {
        close_menu(menu);
        dialog.set(Some(which));
    };
    view! {
        <Show when=move || !elsewhere() && actions().rename>
            <button type="button" class="rename-pen" title="Rename the project"
                aria-label=move || format!("Rename the project “{}”", name())
                on:click=move |_| dialog.set(Some(Dialog::Rename))>
                <PenIcon/>
            </button>
        </Show>
        <Show when=move || !elsewhere()>
            <details class="project-actions" data-menu node_ref=menu
                on:keydown=move |event| if event.key() == "Escape" { close_menu(menu); }>
                <summary class="icon-button" aria-label=move || format!("Actions on the project “{}”", name())
                    title="Project actions">"⋯"</summary>
                <div class="menu" role="menu" aria-label="Project actions">
                    <Show when=move || actions().rename>
                        <button type="button" role="menuitem" on:click=move |_| pick(Dialog::Rename)>"Rename…"</button>
                    </Show>
                    {importer.filter(|_| editor).map(|importer| view! {
                        <button type="button" role="menuitem"
                            on:click=move |_| { close_menu(menu); importer.open(None); }>"Import catalogues…"</button>
                    })}
                    <button type="button" role="menuitem" on:click=move |_| pick(Dialog::Export)>"Export catalogues…"</button>
                    <Show when=move || actions().leave || actions().delete>
                        <hr/>
                    </Show>
                    <Show when=move || actions().leave>
                        <button type="button" role="menuitem" on:click=move |_| pick(Dialog::Leave)>"Leave project…"</button>
                    </Show>
                    <Show when=move || actions().delete>
                        <button type="button" role="menuitem" class="danger" on:click=move |_| pick(Dialog::Delete)>"Delete project…"</button>
                    </Show>
                </div>
            </details>
        </Show>
        {move || dialog.get().map(|which| view! { <ProjectDialog which dialog/> })}
    }
    .into_any()
}

fn close_menu(menu: NodeRef<Details>) {
    if let Some(menu) = menu.get_untracked() {
        menu.set_open(false);
    }
}

#[component]
pub fn PenIcon() -> impl IntoView {
    view! {
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L8 18l-4 1 1-4z"></path>
        </svg>
    }
}
