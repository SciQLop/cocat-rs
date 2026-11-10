//! The project menu's dialogs: new, rename, leave and delete. Each sends one request and
//! shows the server's refusal in place.

use leptos::html::{self, Input};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{self, ApiError};
use crate::projects::delete_confirmed;
use crate::session::Kind;
use crate::views::controls::{Error, Hint, error_line};
use crate::views::projects::Projects;
use crate::views::transfer::ExportDialog;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialog {
    Create,
    CreateWorkspace,
    Rename,
    Leave,
    Delete,
    Export,
}

/// A modal `<dialog>`: the browser keeps focus inside it and closes it on Escape.
#[component]
pub fn ProjectDialog(which: Dialog, dialog: RwSignal<Option<Dialog>>) -> impl IntoView {
    let node = NodeRef::<html::Dialog>::new();
    Effect::new(move |_| {
        if let Some(node) = node.get() {
            let _ = node.show_modal();
        }
    });
    let body = match which {
        Dialog::Create => view! { <Create dialog kind=Kind::Catalogues/> }.into_any(),
        Dialog::CreateWorkspace => view! { <Create dialog kind=Kind::Workspace/> }.into_any(),
        Dialog::Rename => view! { <Rename dialog/> }.into_any(),
        Dialog::Leave => view! { <Leave dialog/> }.into_any(),
        Dialog::Delete => view! { <Delete dialog/> }.into_any(),
        Dialog::Export => view! { <ExportDialog dialog/> }.into_any(),
    };
    view! {
        <dialog class="project-dialog" node_ref=node on:close=move |_| dialog.set(None)>
            {body}
        </dialog>
    }
}

/// Runs `request`; on success hands its answer to `done`, else shows why in `error`.
fn send<T: 'static>(
    error: Error,
    busy: RwSignal<bool>,
    request: impl Future<Output = Result<T, ApiError>> + 'static,
    done: impl FnOnce(T) + 'static,
) {
    busy.set(true);
    spawn_local(async move {
        match request.await {
            Ok(answer) => done(answer),
            Err(failure) => {
                error.try_set(Some(failure.to_string()));
                busy.try_set(false);
            }
        }
    });
}

fn typed(input: NodeRef<Input>) -> String {
    input
        .get_untracked()
        .map(|input| input.value())
        .unwrap_or_default()
}

/// The page leaves a project it can no longer open; the server picks where to land.
fn open_the_app() {
    let _ = window().location().assign("../app");
}

#[component]
fn Buttons(
    action: &'static str,
    #[prop(optional)] danger: bool,
    busy: RwSignal<bool>,
    #[prop(into, optional)] ready: Option<Signal<bool>>,
    dialog: RwSignal<Option<Dialog>>,
) -> impl IntoView {
    let disabled = move || busy.get() || ready.is_some_and(|ready| !ready.get());
    view! {
        <div class="row-buttons">
            <button type="submit" class="button" class:primary=!danger class:danger=danger
                disabled=disabled>{action}</button>
            <button type="button" class="button" on:click=move |_| dialog.set(None)>"Cancel"</button>
        </div>
    }
}

#[component]
fn Create(dialog: RwSignal<Option<Dialog>>, kind: Kind) -> impl IntoView {
    let projects = expect_context::<Projects>();
    let input = NodeRef::<Input>::new();
    let (error, busy): (Error, _) = (RwSignal::new(None), RwSignal::new(false));
    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let (api, name) = (projects.api(), typed(input));
        send(
            error,
            busy,
            async move { api::create_project(&api, &name, kind).await },
            move |project| projects.created(project),
        );
    };
    view! {
        <form method="dialog" class="name-form" on:submit=submit>
            {match kind {
                Kind::Catalogues => view! {
                    <h2>"New project"</h2>
                    <Hint text="A project holds catalogues you share with the same people." anchor="projects" topic="projects"/>
                }.into_any(),
                Kind::Workspace => view! {
                    <h2>"New SciQLop workspace"</h2>
                    <Hint text="A SciQLop workspace shares its notebooks, scripts and files with the same people. You edit them in SciQLop; the web app shows them." anchor="workspaces" topic="workspaces"/>
                }.into_any(),
            }}
            <label>
                <span class="field-label">"Name"</span>
                <input type="text" node_ref=input required maxlength="200" autofocus
                    on:input=move |_| error.set(None)/>
            </label>
            {error_line(error)}
            <Buttons action="Create" busy dialog/>
        </form>
    }
}

#[component]
fn Rename(dialog: RwSignal<Option<Dialog>>) -> impl IntoView {
    let projects = expect_context::<Projects>();
    let (id, name) = projects
        .open
        .with_untracked(|config| (config.room_id.clone(), config.room_name.clone()));
    let input = NodeRef::<Input>::new();
    let (error, busy): (Error, _) = (RwSignal::new(None), RwSignal::new(false));
    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let (api, id, name) = (projects.api(), id.clone(), typed(input));
        send(
            error,
            busy,
            async move { api::rename_project(&api, &id, &name).await },
            move |project| {
                projects.renamed(project);
                dialog.set(None);
            },
        );
    };
    view! {
        <form method="dialog" class="name-form" on:submit=submit>
            <h2>"Rename project"</h2>
            <label>
                <span class="field-label">"Name"</span>
                <input type="text" node_ref=input value=name required maxlength="200" autofocus
                    on:input=move |_| error.set(None)/>
            </label>
            {error_line(error)}
            <Buttons action="Rename" busy dialog/>
        </form>
    }
}

#[component]
fn Leave(dialog: RwSignal<Option<Dialog>>) -> impl IntoView {
    let projects = expect_context::<Projects>();
    let (id, name) = projects
        .open
        .with_untracked(|config| (config.room_id.clone(), config.room_name.clone()));
    let (error, busy): (Error, _) = (RwSignal::new(None), RwSignal::new(false));
    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let (api, id) = (projects.api(), id.clone());
        send(
            error,
            busy,
            async move { api::leave_project(&api, &id).await },
            |()| open_the_app(),
        );
    };
    view! {
        <form method="dialog" class="name-form" on:submit=submit>
            <h2>{format!("Leave “{name}”?")}</h2>
            <p>"You will no longer see its catalogues. Its owner can invite you again."</p>
            {error_line(error)}
            <Buttons action="Leave project" danger=true busy dialog/>
        </form>
    }
}

#[component]
fn Delete(dialog: RwSignal<Option<Dialog>>) -> impl IntoView {
    let projects = expect_context::<Projects>();
    let (id, name) = projects
        .open
        .with_untracked(|config| (config.room_id.clone(), config.room_name.clone()));
    let typed_name = RwSignal::new(String::new());
    let ready = Signal::derive({
        let name = name.clone();
        move || typed_name.with(|typed| delete_confirmed(typed, &name))
    });
    let (error, busy): (Error, _) = (RwSignal::new(None), RwSignal::new(false));
    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        if !ready.get_untracked() {
            return;
        }
        let (api, id) = (projects.api(), id.clone());
        send(
            error,
            busy,
            async move { api::delete_project(&api, &id).await },
            |()| open_the_app(),
        );
    };
    view! {
        <form method="dialog" class="name-form" on:submit=submit>
            <h2>{format!("Delete “{name}”?")}</h2>
            <p>"This deletes its catalogues, its events and its history, for everyone in it. It cannot be undone."</p>
            <label>
                <span class="field-label">{format!("Type “{name}” to confirm")}</span>
                <input type="text" autocomplete="off" autofocus
                    on:input=move |event| typed_name.set(event_target_value(&event))/>
            </label>
            {error_line(error)}
            <Buttons action="Delete project" danger=true busy ready dialog/>
        </form>
    }
}
