//! The catalogues in the trash, with Restore and Delete for good for editors.

use leptos::prelude::*;

use crate::catalogues::{Named, catalogue_list};
use crate::edit;
use crate::state::Room;
use crate::views::controls::{Error, commit, confirm, error_line};

#[component]
pub fn TrashedCatalogues() -> impl IntoView {
    let room = expect_context::<Room>();
    let reader = room.clone();
    let trashed = Memo::new(move |_| reader.read(|txn| catalogue_list(txn).trashed));
    let error: Error = RwSignal::new(None);
    let can_write = room.can_write();
    let room = StoredValue::new_local(room);
    let entry = move |named: Named| {
        let (restored, purged) = (named.uuid.clone(), named.uuid.clone());
        let question = format!(
            "Delete the catalogue “{}” for good? This cannot be undone.",
            named.name
        );
        let restore = move |_| {
            room.with_value(|room| {
                commit(room, error, |txn| edit::restore_catalogue(txn, &restored))
            })
        };
        let purge = move |_| {
            if confirm(&question) {
                room.with_value(|room| {
                    commit(room, error, |txn| edit::purge_catalogue(txn, &purged))
                });
            }
        };
        view! {
            <li class="trashed">
                <span class="label">{named.name.clone()}</span>
                {can_write.then(|| view! {
                    <button type="button" class="button" on:click=restore>"Restore"</button>
                    <button type="button" class="button danger" on:click=purge>"Delete for good"</button>
                })}
            </li>
        }
    };
    view! {
        <Show when=move || trashed.with(|trashed| !trashed.is_empty())>
            <section class="trashed-catalogues" aria-label="Catalogues in the trash">
                <h2 class="section-label">"Catalogues"</h2>
                <ul class="plain">
                    <For each=move || trashed.get() key=Clone::clone children=entry/>
                </ul>
                {error_line(error)}
            </section>
        </Show>
    }
}
