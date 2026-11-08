//! What every edit control shares: writing and showing the error next to the field,
//! confirming, and a small form that asks for a name.

use cocat_core::time::Timestamp;
use leptos::html::Input;
use leptos::prelude::*;
use yrs::TransactionMut;

use crate::edit::EditError;
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
