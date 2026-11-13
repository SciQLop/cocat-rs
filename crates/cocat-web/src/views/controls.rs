//! What every edit control shares: writing and showing the error next to the field,
//! confirming, and a small form that asks for a name.

use cocat_core::time::Timestamp;
use leptos::html::Input;
use leptos::prelude::*;
use yrs::TransactionMut;

use crate::edit::EditError;
use crate::help;
use crate::state::Room;

/// The error of the last write a control made, shown next to it; `None` once one succeeds.
pub type Error = RwSignal<Option<String>>;

pub fn commit(
    room: &Room,
    error: Error,
    edit: impl FnOnce(&mut TransactionMut<'_>) -> Result<(), EditError>,
) {
    error.set(room.write(edit).err().map(|error| error.to_string()));
}

pub fn error_line(error: Error) -> impl IntoView {
    move || {
        error
            .get()
            .map(|message| view! { <p class="field-error" role="alert">{message}</p> })
    }
}

/// The browser's own dialog: destructive actions ask first.
pub fn confirm(question: &str) -> bool {
    window().confirm_with_message(question).unwrap_or(false)
}

pub fn now() -> Option<Timestamp> {
    Timestamp::from_unix_nanos((js_sys::Date::now() * 1e6) as i64)
}

pub fn go_to(hash: &str) {
    let _ = window().location().set_hash(hash);
}

/// One line of explanation, with a "?" that opens the matching help section in a new tab.
#[component]
pub fn Hint(text: &'static str, anchor: &'static str, topic: &'static str) -> impl IntoView {
    view! {
        <p class="hint">
            {text} " "
            <a class="help-link" href=help::href(anchor) target="_blank" rel="noopener"
                aria-label=format!("Help: {topic}")>"?"</a>
        </p>
    }
}

/// A name input with its submit and cancel buttons. `submit` answers the error to show, if
/// any; the form closes itself through `open` on success or cancel.
#[component]
pub fn NameForm(
    label: &'static str,
    action: &'static str,
    #[prop(into)] initial: String,
    open: RwSignal<bool>,
    submit: impl Fn(String) -> Result<(), EditError> + 'static,
) -> impl IntoView {
    let input = NodeRef::<Input>::new();
    let error: Error = RwSignal::new(None);
    Effect::new(move |_| {
        if let Some(input) = input.get() {
            let _ = input.focus();
        }
    });
    let send = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let name = input.get().map(|input| input.value()).unwrap_or_default();
        match submit(name) {
            Ok(()) => open.set(false),
            Err(failure) => error.set(Some(failure.to_string())),
        }
    };
    view! {
        <form class="name-form" on:submit=send>
            <label>
                <span class="field-label">{label}</span>
                <input type="text" node_ref=input value=initial/>
            </label>
            {error_line(error)}
            <div class="row-buttons">
                <button type="submit" class="button primary">{action}</button>
                <button type="button" class="button" on:click=move |_| open.set(false)>"Cancel"</button>
            </div>
        </form>
    }
}

/// A name edited where it shows, in its place: Enter or Save hands the typed name to `save`,
/// Escape or Cancel closes `open`. The caller closes `open` once saved, or puts the refusal
/// in `error`, shown under the field. `class` sizes it like what it replaces.
#[component]
pub fn NameField(
    #[prop(into)] initial: String,
    label: &'static str,
    open: RwSignal<bool>,
    error: Error,
    save: Callback<String>,
    #[prop(optional)] class: &'static str,
) -> impl IntoView {
    let input = NodeRef::<Input>::new();
    Effect::new(move |_| {
        if let Some(input) = input.get() {
            let _ = input.focus();
            input.select();
        }
    });
    let send = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        save.run(input.get().map(|input| input.value()).unwrap_or_default());
    };
    let escape = move |event: leptos::ev::KeyboardEvent| {
        if event.key() == "Escape" {
            open.set(false);
        }
    };
    view! {
        <div class=format!("name-field {class}")>
            <form on:submit=send>
                <input type="text" node_ref=input value=initial aria-label=label maxlength="200"
                    on:keydown=escape on:input=move |_| error.set(None)/>
                <button type="submit" class="button primary">"Save"</button>
                <button type="button" class="button" on:click=move |_| open.set(false)>"Cancel"</button>
            </form>
            {error_line(error)}
        </div>
    }
}
