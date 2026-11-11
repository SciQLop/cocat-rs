//! Links to catalogues of other projects: the sidebar's "Linked" group, the picker that makes
//! one, and the page that opens the original, live, with the visitor's role there.

use std::collections::{HashMap, HashSet};

use cocat_core::model::{self, Link};
use cocat_core::role::Role;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::ApiError;
use crate::edit;
use crate::links::{
    Listed, ListedCatalogues, Reach, Unread, linkable, project_name, reach, shown_name, sources,
};
use crate::projects::Project;
use crate::route::Route;
use crate::state::Room;
use crate::sync::Status;
use crate::views::controls::go_to;
use crate::views::live_room::{RoomBrowser, open_room};
use crate::views::projects::Projects;

/// Each linked project's catalogues, read once per page: names and whether they still exist.
#[derive(Clone, Copy)]
pub struct Lookups {
    found: RwSignal<HashMap<String, Result<Vec<Listed>, Unread>>>,
    asked: StoredValue<HashSet<String>>,
}

impl Lookups {
    /// Reads the catalogues of every project `room` links into and the visitor is in.
    pub fn provide(room: &Room) -> Self {
        let lookups = Self {
            found: RwSignal::new(HashMap::new()),
            asked: StoredValue::new(HashSet::new()),
        };
        provide_context(lookups);
        let projects = expect_context::<Projects>();
        let reader = room.clone();
        Effect::new(move |_| {
            let links = reader.read(|txn| model::links(txn));
            let api = projects.api();
            projects.list.with(|list| {
                links
                    .iter()
                    .filter(|link| list.iter().any(|project| project.id == link.project))
                    .for_each(|link| lookups.ask(&api, &link.project));
            });
        });
        lookups
    }

    fn ask(self, api: &str, project: &str) {
        if !self
            .asked
            .try_update_value(|asked| asked.insert(project.to_owned()))
            .unwrap_or(false)
        {
            return;
        }
        let (api, project) = (api.to_owned(), project.to_owned());
        spawn_local(async move {
            let read = catalogues_of(&api, &project).await.map_err(unread);
            self.found.update(|found| {
                found.insert(project, read);
            });
        });
    }

    /// A fresher list of `project`'s catalogues, read by the picker.
    fn known(self, project: &str, listed: Vec<Listed>) {
        self.asked
            .update_value(|asked| _ = asked.insert(project.to_owned()));
        self.found.update(|found| {
            found.insert(project.to_owned(), Ok(listed));
        });
    }

    fn reach(self, link: &Link, projects: &[Project]) -> Reach {
        self.found
            .with(|found| reach(link, projects, found.get(&link.project)))
    }
}

pub async fn catalogues_of(api: &str, project: &str) -> Result<Vec<Listed>, ApiError> {
    crate::api::read::<ListedCatalogues>(api, &["rooms", project, "catalogues"])
        .await
        .map(|listed| listed.catalogues)
}

/// Only a missing answer is temporary; any refusal means the catalogues are out of reach.
fn unread(error: ApiError) -> Unread {
    match error {
        ApiError::Network(_) => Unread::Unreachable,
        _ => Unread::Refused,
    }
}

/// The sidebar's links, under the catalogues; nothing when the project has none.
#[component]
pub fn LinkedSection(shown: Memo<Route>, page: String) -> impl IntoView {
    let room = expect_context::<Room>();
    let links = Memo::new(move |_| room.read(|txn| model::links(txn)));
    let page = StoredValue::new(page);
    view! {
        <Show when=move || links.with(|links| !links.is_empty())>
            <section class="linked-section" aria-label="Linked catalogues">
                <h2 class="eyebrow">"Linked"</h2>
                <For each=move || links.get() key=|link| (link.id.clone(), link.name.clone()) let:link>
                    <LinkEntry link shown page=page.get_value()/>
                </For>
            </section>
        </Show>
    }
}

#[component]
fn LinkEntry(link: Link, shown: Memo<Route>, page: String) -> impl IntoView {
    let room = expect_context::<Room>();
    let projects = expect_context::<Projects>();
    let lookups = expect_context::<Lookups>();
    let route = Route::Link(link.id.clone());
    let href = route.href(&page);
    let state = {
        let link = link.clone();
        Memo::new(move |_| projects.list.with(|list| lookups.reach(&link, list)))
    };
    let name = {
        let link = link.clone();
        move || state.with(|state| shown_name(&link, state))
    };
    let from = {
        let link = link.clone();
        move || {
            format!(
                "from {}",
                projects.list.with(|list| project_name(&link, list))
            )
        }
    };
    let gone = move || state.with(|state| *state == Reach::Gone);
    let menu = NodeRef::<leptos::html::Details>::new();
    let editor = room.can_write();
    let id = StoredValue::new(link.id.clone());
    let remove = move |_| {
        if let Some(menu) = menu.get_untracked() {
            menu.set_open(false);
        }
        unlink(
            &room,
            &id.get_value(),
            shown.get_untracked() == Route::Link(id.get_value()),
        );
    };
    view! {
        <div class="entry">
            <a class="catalogue linked" class:gone=gone href=href
                aria-current=move || (shown.get() == route).then_some("page")
                title=move || gone().then_some("No longer available")>
                <span class="name">
                    <LinkIcon/>
                    <span class="label"><span class="link-name">{name}</span><small class="from">{from}</small></span>
                </span>
            </a>
            {editor.then(|| view! {
                <details class="entry-menu" data-menu node_ref=menu>
                    <summary aria-label=format!("Actions for the link “{}”", link.name)>"⋯"</summary>
                    <div class="menu" role="menu">
                        <button type="button" role="menuitem" on:click=remove>"Remove link"</button>
                    </div>
                </details>
            })}
        </div>
    }
}

/// Removes link `id`; the page leaves it when it was on screen.
fn unlink(room: &Room, id: &str, on_screen: bool) {
    if room.write(|txn| edit::unlink(txn, id)).is_ok() && on_screen {
        go_to("");
    }
}

/// Whether project `id` is the visitor's own ("My catalogues"), the only place links go.
pub fn is_personal(projects: Projects, id: &str) -> bool {
    projects.list.with(|list| {
        list.iter()
            .any(|project| project.id == id && project.personal)
    })
}

/// The form that links a catalogue: a project of the visitor's, then one of its catalogues.
#[component]
pub fn LinkPicker(open: RwSignal<bool>) -> impl IntoView {
    let room = expect_context::<Room>();
    let projects = expect_context::<Projects>();
    let lookups = expect_context::<Lookups>();
    let here = room.config.room_id.clone();
    let offered = Memo::new(move |_| projects.list.with(|list| sources(list, &here)));
    let source = RwSignal::new(None::<Project>);
    let catalogues = RwSignal::new(None::<Result<Vec<Listed>, String>>);
    let picked = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let reader = room.clone();
    let choices = Memo::new(move |_| {
        let links = reader.read(|txn| model::links(txn));
        let project = source.with(|source| source.as_ref().map(|p| p.id.clone()))?;
        catalogues.with(|read| match read.as_ref()? {
            Ok(listed) => Some(Ok(linkable(listed, &links, &project))),
            Err(why) => Some(Err(why.clone())),
        })
    });
    let pick_project = move |event: leptos::ev::Event| {
        let id = event_target_value(&event);
        let project =
            offered.with_untracked(|offered| offered.iter().find(|p| p.id == id).cloned());
        source.set(project.clone());
        catalogues.set(None);
        picked.set(String::new());
        let Some(project) = project else { return };
        let api = projects.api();
        spawn_local(async move {
            let read = catalogues_of(&api, &project.id).await;
            if let Ok(listed) = &read {
                lookups.known(&project.id, listed.clone());
            }
            if source.with_untracked(|now| now.as_ref() == Some(&project)) {
                catalogues.set(Some(read.map_err(|error| error.to_string())));
            }
        });
    };
    let writer = room.clone();
    let send = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let (Some(project), uuid) = (source.get_untracked(), picked.get_untracked()) else {
            return;
        };
        let name = choices.with_untracked(|choices| {
            let listed = choices.as_ref()?.as_ref().ok()?;
            listed
                .iter()
                .find(|c| c.uuid == uuid)
                .map(|c| c.name.clone())
        });
        let Some(name) = name else {
            error.set(Some("Pick a catalogue to link.".to_owned()));
            return;
        };
        let personal = is_personal(projects, &writer.config.room_id);
        match writer.write(|txn| {
            edit::link_catalogue(txn, &writer.config, personal, &project, &uuid, &name)
        }) {
            Ok(id) => {
                open.set(false);
                go_to(&Route::Link(id).hash());
            }
            Err(failure) => error.set(Some(failure.to_string())),
        }
    };
    let catalogue_field = move || {
        match choices.get() {
        None if source.with(Option::is_some) => view! { <p class="quiet">"Reading its catalogues…"</p> }.into_any(),
        None => ().into_any(),
        Some(Err(why)) => view! { <p class="field-error" role="alert">{why}</p> }.into_any(),
        Some(Ok(listed)) if listed.is_empty() => {
            view! { <p class="quiet">"Nothing left to link there."</p> }.into_any()
        }
        Some(Ok(listed)) => view! {
            <label>
                <span class="field-label">"Catalogue"</span>
                <select on:change=move |event| picked.set(event_target_value(&event))>
                    <option value="" selected=true disabled=true>"Pick a catalogue"</option>
                    {listed.into_iter().map(|c| view! { <option value=c.uuid>{c.name}</option> }).collect_view()}
                </select>
            </label>
        }
        .into_any(),
    }
    };
    view! {
        <form class="name-form link-picker" on:submit=send>
            <p class="quiet">"A link shows a catalogue of another project here. Changes go to the original."</p>
            {move || if offered.with(Vec::is_empty) {
                view! { <p class="quiet">"You are in no other project to link from."</p> }.into_any()
            } else {
                view! {
                    <label>
                        <span class="field-label">"Project"</span>
                        <select on:change=pick_project>
                            <option value="" selected=true disabled=true>"Pick a project"</option>
                            {offered.get().into_iter().map(|p| view! { <option value=p.id>{p.name}</option> }).collect_view()}
                        </select>
                    </label>
                }.into_any()
            }}
            {catalogue_field}
            {move || error.get().map(|why| view! { <p class="field-error" role="alert">{why}</p> })}
            <div class="row-buttons">
                <button type="submit" class="button primary" disabled=move || picked.with(String::is_empty)>"Link"</button>
                <button type="button" class="button" on:click=move |_| open.set(false)>"Cancel"</button>
            </div>
        </form>
    }
}

/// What the main pane shows for a link: the original, or why it does not open.
#[derive(Debug, Clone, PartialEq)]
enum Pane {
    Missing,
    Loading,
    Gone(String),
    Unreachable(String),
    /// Only what the room depends on: a renamed project keeps its connection.
    Open {
        project: String,
        role: Role,
        uuid: String,
    },
}

#[component]
pub fn LinkedCatalogue(id: String) -> impl IntoView {
    let room = expect_context::<Room>();
    let projects = expect_context::<Projects>();
    let lookups = expect_context::<Lookups>();
    let reader = room.clone();
    let link_id = id.clone();
    let pane = Memo::new(move |_| {
        let links = reader.read(|txn| model::links(txn));
        let Some(link) = links.into_iter().find(|link| link.id == link_id) else {
            return Pane::Missing;
        };
        projects.list.with(|list| match lookups.reach(&link, list) {
            Reach::Loading => Pane::Loading,
            Reach::Gone => Pane::Gone(project_name(&link, list)),
            Reach::Unreachable => Pane::Unreachable(project_name(&link, list)),
            Reach::Open { project, .. } => Pane::Open {
                project: project.id,
                role: project.role,
                uuid: link.catalogue.clone(),
            },
        })
    });
    let listed = move |id: &str| {
        projects
            .list
            .with_untracked(|list| list.iter().find(|project| project.id == id).cloned())
    };
    move || {
        match pane.get() {
        Pane::Missing => gone_view("This link is no longer in the project.".to_owned(), None),
        Pane::Unreachable(project) => gone_view(format!("{project} cannot be reached now. Reload the page to try again."), None),
        Pane::Loading => view! { <main class="content"><p class="quiet">"Opening the linked catalogue…"</p></main> }.into_any(),
        Pane::Gone(project) => gone_view(no_longer(&project), Some((room.clone(), id.clone()))),
        Pane::Open { project, uuid, .. } => match listed(&project) {
            Some(project) => view! { <Original project uuid id=id.clone()/> }.into_any(),
            None => gone_view(no_longer(&project), Some((room.clone(), id.clone()))),
        },
    }
    }
}

fn no_longer(project: &str) -> String {
    format!("No longer available: you are not in {project} any more, or the catalogue was deleted.")
}

/// Why a link does not open, with Remove link (of link `id` in `room`) for its editors.
fn gone_view(text: String, removable: Option<(Room, String)>) -> AnyView {
    let remove = removable.filter(|(room, _)| room.can_write()).map(|(room, id)| {
        view! { <button type="button" class="button" on:click=move |_| unlink(&room, &id, true)>"Remove link"</button> }
    });
    view! {
        <main class="content">
            <div class="link-gone" role="status">
                <p>{text}</p>
                {remove}
            </div>
        </main>
    }
    .into_any()
}

/// The original catalogue, live: a room of its own for this subtree, with the visitor's role
/// in the original project.
#[component]
fn Original(project: Project, uuid: String, id: String) -> impl IntoView {
    let outer = expect_context::<Room>();
    let Some(config) = outer.config.for_project(&project) else {
        return gone_view(
            format!("{} cannot be opened from this page.", project.name),
            None,
        );
    };
    provide_context(LinkedFrom(project.clone()));
    let room = match open_room(config) {
        Ok(room) => room,
        Err(error) => {
            return view! { <main class="content"><p role="alert">{format!("The linked catalogue cannot sync: {error}")}</p></main> }
                .into_any();
        }
    };
    let here = Route::Catalogue(uuid.clone());
    let shown = Memo::new(move |_| here.clone());
    let reader = room.clone();
    let deleted = Memo::new(move |_| {
        let live = reader.status.get() == Status::Live;
        live && reader.read(|txn| model::catalogue(txn, &uuid).is_none_or(|found| found.deleted))
    });
    let name = project.name.clone();
    (move || {
        if deleted.get() {
            return gone_view(no_longer(&name), Some((outer.clone(), id.clone())));
        }
        let from = name.clone();
        view! {
            <RoomBrowser shown>
                <p class="linked-from">"Linked from " <strong>{from}</strong> ": changes go there."</p>
            </RoomBrowser>
        }
        .into_any()
    })
    .into_any()
}

/// Inside a link: the original project, where a catalogue made there opens.
#[derive(Clone)]
pub struct LinkedFrom(pub Project);

/// Opens catalogue `uuid` just made in the room on screen: here, or in the original
/// project when the page shows a link. Call it while building the view, not in a handler.
pub fn opener() -> impl Fn(String) + Clone + 'static {
    let linked = use_context::<LinkedFrom>();
    let projects = use_context::<Projects>();
    move |uuid: String| {
        let view = Route::Catalogue(uuid);
        match (&linked, projects) {
            (Some(LinkedFrom(project)), Some(projects)) => projects.switch_to(project, Some(&view)),
            _ => go_to(&view.hash()),
        }
    }
}

#[component]
fn LinkIcon() -> impl IntoView {
    view! {
        <svg class="link-mark" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
            stroke-width="2" role="img" aria-label="Linked catalogue">
            <path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1"></path>
            <path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"></path>
        </svg>
    }
}
