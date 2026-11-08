//! The project menu in the header, and switching the page to another project.

use leptos::html::Details;
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsValue;

use crate::api;
use crate::history::browse_href;
use crate::projects::{Actions, Arrival, Project, arrival, title};
use crate::route::Route;
use crate::session::RoomConfig;
use crate::views::controls::{Hint, go_to};
use crate::views::notice::Notice;
use crate::views::project_dialogs::{Dialog, ProjectDialog};

/// The visitor's projects and the one the page holds. The page shows the open one; changing
/// its id brings up a fresh room, connection and views. The notice is about the project the
/// page opened with, so it goes when another one opens.
#[derive(Clone, Copy)]
pub struct Projects {
    pub list: RwSignal<Vec<Project>>,
    pub open: RwSignal<RoomConfig>,
    pub notice: RwSignal<Option<Notice>>,
}

impl Projects {
    pub fn new(config: RoomConfig, notice: Option<Notice>) -> Self {
        set_title(&config.room_name);
        Self {
            list: RwSignal::new(Vec::new()),
            open: RwSignal::new(config),
            notice: RwSignal::new(notice),
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
        if self
            .open
            .with_untracked(|config| config.room_id == project.id)
        {
            return;
        }
        let Some(next) = self
            .open
            .with_untracked(|config| config.for_project(project))
        else {
            return;
        };
        if let Ok(history) = window().history() {
            let _ =
                history.push_state_with_url(&JsValue::NULL, "", Some(&browse_href(&project.id)));
        }
        self.show(next);
    }

    /// The browser went back or forward.
    pub fn arrived(self) {
        let path = window().location().pathname().unwrap_or_default();
        let open = self.open.with_untracked(|config| config.room_id.clone());
        match self
            .list
            .with_untracked(|listed| arrival(&open, &path, listed))
        {
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
        set_title(&next.room_name);
        self.notice.set(None);
        self.open.set(next);
    }
}

fn set_title(name: &str) {
    document().set_title(&title(name));
}

/// The open project's name, as a button that lists the other projects and what can be done
/// with this one.
#[component]
pub fn ProjectMenu() -> impl IntoView {
    let projects = expect_context::<Projects>();
    let menu = NodeRef::<Details>::new();
    let dialog = RwSignal::new(None::<Dialog>);
    let open_id = move || projects.open.with(|config| config.room_id.clone());
    let name = move || projects.open.with(|config| config.room_name.clone());
    let actions = move || {
        projects
            .open
            .with(|config| Actions::on(&config.room_id, config.role))
    };
    let close = move || {
        if let Some(menu) = menu.get_untracked() {
            menu.set_open(false);
        }
    };
    let pick = move |which: Dialog| {
        close();
        dialog.set(Some(which));
    };
    let toggled = move |_| {
        if menu.get_untracked().is_some_and(|menu| menu.open()) {
            projects.reload();
        }
    };
    let entry = move |project: Project| {
        let here = project.id == open_id();
        let role = project.role.as_str();
        let target = project.clone();
        view! {
            <button type="button" role="menuitem" class="project-entry"
                aria-current=here.then_some("true")
                on:click=move |_| { close(); projects.switch(&target); }>
                <span class="label">{project.name}</span>
                <small>{role}</small>
            </button>
        }
    };
    view! {
        <details class="project-menu" node_ref=menu on:toggle=toggled
            on:keydown=move |event| if event.key() == "Escape" { close(); }>
            <summary class="project-switch" aria-label=move || format!("Project {}, open the project menu", name())>
                <span class="label">{name}</span>
                <span class="caret" aria-hidden="true">"▾"</span>
            </summary>
            <div class="menu" role="menu" aria-label="Projects">
                <button type="button" role="menuitem" class="project-entry public-entry"
                    on:click=move |_| { close(); go_to(&Route::Public.hash()); }>
                    <span class="label">"Public catalogues"</span>
                    <small>"everyone"</small>
                </button>
                <hr/>
                <p class="eyebrow">"Projects"</p>
                <For each=move || projects.list.get() key=|project| (project.id.clone(), project.name.clone(), project.role) children=entry/>
                <hr/>
                <button type="button" role="menuitem" on:click=move |_| pick(Dialog::Create)>"+ New project"</button>
                <Show when=move || actions().rename>
                    <button type="button" role="menuitem" on:click=move |_| pick(Dialog::Rename)>
                        {move || format!("Rename “{}”…", name())}
                    </button>
                </Show>
                <Show when=move || actions().leave>
                    <button type="button" role="menuitem" on:click=move |_| pick(Dialog::Leave)>"Leave project…"</button>
                </Show>
                <Show when=move || actions().delete>
                    <button type="button" role="menuitem" class="danger" on:click=move |_| pick(Dialog::Delete)>"Delete project…"</button>
                </Show>
                <hr/>
                <button type="button" role="menuitem" on:click=move |_| { close(); go_to(&Route::Groups.hash()); }>"Groups…"</button>
                <Hint text="A project is a set of catalogues shared with the same people." anchor="projects" topic="projects"/>
            </div>
        </details>
        {move || dialog.get().map(|which| view! { <ProjectDialog which dialog/> })}
    }
}
