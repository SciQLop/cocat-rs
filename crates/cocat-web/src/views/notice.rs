//! A word above the app: the server's on the last action of an old page's form, or the
//! page's own after an action that can be taken back.

use leptos::prelude::*;

use crate::edit;
use crate::route::Route;
use crate::state::Room;
use crate::views::controls::go_to;
use crate::views::invite::CopyLink;
use crate::views::projects::Projects;

#[derive(Debug, Clone)]
pub struct Notice {
    pub text: String,
    /// A share link the action made: the only time it is shown.
    pub link: Option<String>,
    /// What the Undo button takes back; the notice goes with it.
    pub undo: Option<Undo>,
}

/// An action the notice bar can take back. It holds data, not a closure: the component that
/// acted is often gone by then (moving the catalogue on screen to the trash leaves its page),
/// and the bar does the undoing in the open project.
#[derive(Debug, Clone)]
pub enum Undo {
    /// Restores this catalogue from the trash and shows it.
    Trashed(String),
}

fn undo(room: &Room, what: &Undo) {
    match what {
        Undo::Trashed(uuid) => {
            if room.write(|txn| edit::restore_catalogue(txn, uuid)).is_ok() {
                go_to(&Route::Catalogue(uuid.clone()).hash());
            }
        }
    }
}

#[component]
pub fn NoticeBar() -> impl IntoView {
    let notice = expect_context::<Projects>().notice;
    let room = use_context::<Room>().map(StoredValue::new_local);
    move || {
        notice.get().map(|shown| {
            view! {
                <div class="notice-bar" role="status">
                    <div class="grow">
                        <p>{shown.text}</p>
                        {shown.link.map(|link| view! { <CopyLink link/> })}
                    </div>
                    {shown.undo.zip(room).map(|(what, room)| view! {
                        <button type="button" class="button" on:click=move |_| {
                            notice.set(None);
                            room.with_value(|room| undo(room, &what));
                        }>"Undo"</button>
                    })}
                    <button type="button" class="icon" aria-label="Dismiss" on:click=move |_| notice.set(None)>"✕"</button>
                </div>
            }
        })
    }
}
