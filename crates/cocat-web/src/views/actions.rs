//! What an editor can do to the catalogue on screen: add an event, edit the rule,
//! freeze it, move it to the trash.

use cocat_core::model;
use cocat_core::time::Timestamp;
use leptos::prelude::*;

use crate::edit;
use crate::route::Route;
use crate::rows::{self, Row};
use crate::state::Room;
use crate::submissions::{Proposal, PublicAction, menu_proposes, proposal_for, public_actions};
use crate::views::controls::{Error, NameForm, confirm, error_line, go_to, now};
use cocat_core::room_id::is_public_id;

/// Which form under the heading is open.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Form {
    Closed,
    Rule,
    Freeze,
    Submit,
    Update,
    Propose,
    Copy,
    Export,
}

#[component]
pub fn Actions(
    shown: Memo<Route>,
    selected: RwSignal<Option<Row>>,
    form: RwSignal<Form>,
    proposals: RwSignal<Vec<Proposal>>,
) -> impl IntoView {
    let room_id = expect_context::<Room>().config.room_id.clone();
    let public = is_public_id(&room_id);
    let relayed = crate::views::public::relay_of(room_id);
    let editor = expect_context::<Room>().can_write();
    let room = StoredValue::new_local(expect_context::<Room>());
    let error: Error = RwSignal::new(None);
    let catalogue = Memo::new(move |_| match shown.get() {
        Route::Catalogue(uuid) => Some(uuid),
        _ => None,
    });
    let smart = Memo::new(move |_| {
        let uuid = catalogue.get()?;
        let found = room.with_value(|room| room.read(|txn| model::catalogue(txn, &uuid)));
        found.map(|found| found.rule.is_some())
    });
    let new_event = move |_| {
        if let (Some(uuid), Some(now)) = (catalogue.get_untracked(), now()) {
            room.with_value(|room| add_event(room, &uuid, now, selected, error));
        }
    };
    let trash = move |_| {
        if let Some(uuid) = catalogue.get_untracked() {
            room.with_value(|room| {
                if let Err(failure) = trash_catalogue(room, &uuid, true) {
                    error.set(Some(failure));
                }
            });
        }
    };
    let linked = move || {
        let uuid = catalogue.get()?;
        proposals.with(|listed| proposal_for(listed, &uuid).map(|proposal| proposal.name.clone()))
    };
    let menu = NodeRef::<leptos::html::Details>::new();
    let toggle = move |which: Form| {
        form.update(|open| *open = if *open == which { Form::Closed } else { which });
        if let Some(menu) = menu.get_untracked() {
            let _ = menu.remove_attribute("open");
        }
    };
    view! {
        <Show when=move || smart.get().is_some()>
            <details class="menu-button" data-menu node_ref=menu>
                <summary aria-label="More actions">"More"</summary>
                <div class="menu">
                    {editor.then(|| view! {
                        <button type="button" on:click=move |_| toggle(Form::Rule)>"Rule as JSON…"</button>
                        <Show when=move || smart.get() == Some(true)>
                            <button type="button" on:click=move |_| toggle(Form::Freeze)>"Freeze…"</button>
                        </Show>
                    })}
                    <button type="button" on:click=move |_| toggle(Form::Export)>"Export…"</button>
                    {move || menu_proposes(public, relayed.with(Option::is_some)).then(|| view! {
                        <button type="button" on:click=move |_| toggle(Form::Propose)>"Propose a change…"</button>
                    })}
                    {(editor && !public).then(|| view! {
                        <button type="button" on:click=move |_| toggle(Form::Submit)>"Submit to Public catalogues…"</button>
                    })}
                    {move || linked().filter(|_| editor).map(|name| view! {
                        <button type="button" on:click=move |_| toggle(Form::Update)>{format!("Submit as an update to “{name}”…")}</button>
                    })}
                    {editor.then(|| view! {
                        <button type="button" class="danger" on:click=trash>"Delete catalogue…"</button>
                    })}
                </div>
            </details>
            {editor.then(|| view! {
                <button type="button" class="button primary" on:click=new_event>"New event"</button>
            })}
        </Show>
        {error_line(error)}
    }
}

/// In a public catalogue: copy it to use it, and for its readers, copy it to change it
/// and submit the change.
#[component]
pub fn PublicButtons(shown: Memo<Route>, form: RwSignal<Form>, editor: bool) -> impl IntoView {
    let on_catalogue = move || matches!(shown.get(), Route::Catalogue(_));
    let id = expect_context::<Room>().config.room_id.clone();
    let relay = crate::views::public::relay_of(id);
    let buttons = move || {
        public_actions(true, editor, relay.with(Option::is_some)).iter().map(|&(action, main)| {
        let which = match action {
            PublicAction::Copy => Form::Copy,
            PublicAction::Propose => Form::Propose,
        };
        let toggle = move |_| form.update(|open| *open = if *open == which { Form::Closed } else { which });
        view! {
            <button type="button" class="button" class:primary=main aria-pressed=move || (form.get() == which).to_string()
                on:click=toggle>{action.label()}</button>
        }
    }).collect_view()
    };
    view! { <Show when=on_catalogue>{buttons}</Show> }
}

/// A new event in the catalogue, selected so its panel opens.
fn add_event(
    room: &Room,
    catalogue: &str,
    now: Timestamp,
    selected: RwSignal<Option<Row>>,
    error: Error,
) {
    let author = room.config.user_email.clone();
    match room.write(|txn| edit::new_event(txn, Some(catalogue), now, &author)) {
        Ok(event) => {
            error.set(None);
            selected.set(room.read_untracked(|txn| rows::event(txn, &event)));
        }
        Err(failure) => error.set(Some(failure.to_string())),
    }
}

/// Asks, then moves the catalogue to the trash; `shown`: it is the one on screen, so the page
/// moves on to the first catalogue.
pub fn trash_catalogue(room: &Room, uuid: &str, shown: bool) -> Result<(), String> {
    let name = room
        .read_untracked(|txn| model::catalogue(txn, uuid))
        .map(|found| found.name)
        .unwrap_or_default();
    if !confirm(&format!(
        "Delete the catalogue “{name}”? Its events stay in the project. The catalogue waits in \
         the trash, where it can be restored."
    )) {
        return Ok(());
    }
    room.write(|txn| edit::trash_catalogue(txn, uuid))
        .map_err(|failure| failure.to_string())?;
    if shown {
        go_to("");
    }
    Ok(())
}

/// Asks the name of the normal catalogue to freeze the smart one on screen into, then shows it.
#[component]
pub fn Freeze(uuid: String, form: RwSignal<Form>) -> impl IntoView {
    let room = expect_context::<Room>();
    let name = room
        .read_untracked(|txn| model::catalogue(txn, &uuid))
        .map(|found| format!("{} (frozen)", found.name))
        .unwrap_or_default();
    let open = RwSignal::new(true);
    Effect::new(move |_| {
        if !open.get() {
            form.set(Form::Closed);
        }
    });
    let show = crate::views::links::opener();
    let submit = move |name: String| {
        let made = room.write(|txn| edit::materialize(txn, &uuid, &name, &room.config))?;
        show(made);
        Ok(())
    };
    view! {
        <div class="inline-form">
            <p class="quiet">"A normal catalogue holding the events this one picks now. Joined intervals become events you can edit. This smart catalogue stays as it is."</p>
            <NameForm label="Name of the new catalogue" action="Freeze" initial=name open submit/>
        </div>
    }
}
