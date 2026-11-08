use leptos::prelude::*;

use crate::catalogues::{CatalogueItem, catalogue_list};
use crate::route::Route;
use crate::state::Room;

#[component]
pub fn Sidebar(shown: Memo<Route>) -> impl IntoView {
    let room = expect_context::<Room>();
    let list = Memo::new(move |_| room.read(|txn| catalogue_list(txn)));
    view! {
        <aside class="sidebar" aria-label="Catalogues">
            <h2 class="eyebrow">"Catalogues"</h2>
            <nav class="catalogue-list">
                <For each=move || list.get().catalogues key=Clone::clone let:item>
                    <Entry item shown/>
                </For>
                <Show when=move || list.with(|list| list.catalogues.is_empty())>
                    <p class="quiet">"No catalogues in this room yet."</p>
                </Show>
            </nav>
            <a
                class="catalogue trash"
                href=Route::Trash.hash()
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

#[component]
fn Entry(item: CatalogueItem, shown: Memo<Route>) -> impl IntoView {
    let route = Route::Catalogue(item.uuid.clone());
    let href = route.hash();
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
