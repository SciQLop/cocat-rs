use leptos::prelude::*;

use crate::catalogues::{CatalogueItem, catalogue_list};
use crate::edit;
use crate::route::Route;
use crate::state::Room;
use crate::views::controls::{NameForm, go_to};

#[component]
pub fn Sidebar(shown: Memo<Route>) -> impl IntoView {
    let room = expect_context::<Room>();
    let reader = room.clone();
    let list = Memo::new(move |_| reader.read(|txn| catalogue_list(txn)));
    let page = window().location().pathname().unwrap_or_default();
    let trash = Route::Trash.href(&page);
    view! {
        <aside class="sidebar" aria-label="Catalogues">
            <div class="sidebar-head">
                <h2 class="eyebrow">"Catalogues"</h2>
                {room.can_write().then(|| view! { <NewCatalogue/> })}
            </div>
            <nav class="catalogue-list">
                <For each=move || list.get().catalogues key=Clone::clone let:item>
                    <Entry item shown page=page.clone()/>
                </For>
                <Show when=move || list.with(|list| list.catalogues.is_empty())>
                    <p class="quiet">"No catalogues in this project yet."</p>
                </Show>
            </nav>
            <a
                class="catalogue trash"
                href=trash
                aria-current=move || current(shown.get() == Route::Trash)
            >
                <span class="name">
                    <TrashIcon/>
                    "Trash"
                </span>
                <span class="count">{move || list.get().trash}</span>
            </a>
        </aside>
    }
}

/// The "+" of the list, and the form it opens.
#[component]
fn NewCatalogue() -> impl IntoView {
    let room = expect_context::<Room>();
    let open = RwSignal::new(false);
    let submit = move |name: String| {
        let author = room.config.user_email.clone();
        let made = room.write(|txn| edit::new_catalogue(txn, &name, &author))?;
        go_to(&Route::Catalogue(made).hash());
        Ok(())
    };
    let submit = StoredValue::new_local(submit);
    view! {
        <button type="button" class="icon add-catalogue" aria-label="New catalogue"
            aria-expanded=move || open.get().to_string() on:click=move |_| open.update(|open| *open = !*open)>
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                stroke-width="2" aria-hidden="true"><path d="M12 5v14M5 12h14"></path></svg>
        </button>
        <Show when=move || open.get()>
            <div class="sidebar-form">
                <NameForm label="Name of the new catalogue" action="Create" initial="" open
                    submit=move |name| submit.with_value(|submit| submit(name))/>
            </div>
        </Show>
    }
}

#[component]
fn Entry(item: CatalogueItem, shown: Memo<Route>, page: String) -> impl IntoView {
    let route = Route::Catalogue(item.uuid.clone());
    let href = route.href(&page);
    let count = item
        .count
        .map_or_else(|| "!".to_owned(), |count| count.to_string());
    let count_title = item.count.is_none().then_some("The rule does not evaluate");
    view! {
        <a class="catalogue" href=href aria-current=move || current(shown.get() == route)>
            <span class="name">
                <Show when=move || item.smart>
                    <SmartIcon/>
                </Show>
                <span class="label">{item.name.clone()}</span>
            </span>
            <span class="count" title=count_title>{count}</span>
        </a>
    }
}

fn current(selected: bool) -> Option<&'static str> {
    selected.then_some("page")
}

#[component]
fn SmartIcon() -> impl IntoView {
    view! {
        <svg class="smart" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
            stroke-width="2" role="img" aria-label="Smart catalogue">
            <path d="M4 6h16M7 12h10M10 18h4"></path>
        </svg>
    }
}

#[component]
fn TrashIcon() -> impl IntoView {
    view! {
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
            stroke-width="2" aria-hidden="true">
            <path d="M3 6h18M8 6V4h8v2M6 6l1 14h10l1-14"></path>
        </svg>
    }
}
