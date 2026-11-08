//! What an editor can do to the catalogue on screen: add an event, edit the rule,
//! freeze it, move it to the trash.

use cocat_core::model;
use cocat_core::time::Timestamp;
use leptos::prelude::*;

use crate::edit;
use crate::route::Route;
use crate::rows::{self, Row};
use crate::state::Room;
use crate::views::controls::{Error, NameForm, confirm, error_line, go_to, now};

/// Which form under the heading is open.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Form {
    Closed,
    Rule,
    Freeze,
}

#[component]
pub fn Actions(
    shown: Memo<Route>,
    selected: RwSignal<Option<Row>>,
    form: RwSignal<Form>,
) -> impl IntoView {
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
            room.with_value(|room| trash_catalogue(room, &uuid, error));
        }
    };
    let toggle = move |which: Form| {
        form.update(|open| *open = if *open == which { Form::Closed } else { which });
    };
    view! {
        <Show when=move || smart.get() == Some(true)>
            <button type="button" class="button" aria-pressed=move || (form.get() == Form::Rule).to_string()
                on:click=move |_| toggle(Form::Rule)>"Edit rule"</button>
        </Show>
        <Show when=move || smart.get().is_some()>
            <details class="menu-button">
                <summary aria-label="More actions">"More"</summary>
                <div class="menu">
                    <Show when=move || smart.get() == Some(true)>
                        <button type="button" on:click=move |_| toggle(Form::Freeze)>"Freeze…"</button>
                    </Show>
                    <button type="button" class="danger" on:click=trash>"Move catalogue to trash…"</button>
                </div>
            </details>
            <button type="button" class="button primary" on:click=new_event>"New event"</button>
        </Show>
        {error_line(error)}
    }
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

fn trash_catalogue(room: &Room, uuid: &str, error: Error) {
    let name = room
        .read_untracked(|txn| model::catalogue(txn, uuid))
        .map(|found| found.name)
        .unwrap_or_default();
    if !confirm(&format!(
        "Move the catalogue “{name}” to the trash? It can be restored from there."
    )) {
        return;
    }
    match room.write(|txn| edit::trash_catalogue(txn, uuid)) {
        Ok(()) => go_to(""),
        Err(failure) => error.set(Some(failure.to_string())),
    }
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
    let submit = move |name: String| {
        let author = room.config.user_email.clone();
        let made = room.write(|txn| edit::materialize(txn, &uuid, &name, &author))?;
        go_to(&Route::Catalogue(made).hash());
        Ok(())
    };
    view! {
        <div class="inline-form">
            <p class="quiet">"A normal catalogue holding the events this one picks now. Joined intervals become events you can edit. This smart catalogue stays as it is."</p>
            <NameForm label="Name of the new catalogue" action="Freeze" initial=name open submit/>
        </div>
    }
}
