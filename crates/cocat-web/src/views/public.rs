//! The index of the public catalogues: a search box over shelves, one per folder. A row opens
//! its catalogue like any project.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::public::{PublicCatalogue, Shelf, shelves};
use crate::route::Route;
use crate::views::controls::{Error, Hint, error_line};
use crate::views::projects::Projects;

#[component]
pub fn PublicPage() -> impl IntoView {
    let projects = expect_context::<Projects>();
    let all = RwSignal::new(None::<Vec<PublicCatalogue>>);
    let search = RwSignal::new(String::new());
    let error: Error = RwSignal::new(None);
    let api = projects.api();
    spawn_local(async move {
        match api::read::<Vec<PublicCatalogue>>(&api, &["public"]).await {
            Ok(listed) => {
                all.try_set(Some(listed));
            }
            Err(failure) => {
                error.try_set(Some(failure.to_string()));
            }
        }
    });
    let page = window().location().pathname().unwrap_or_default();
    let shown = move || {
        all.with(|all| {
            all.as_ref()
                .map(|all| (all.is_empty(), search.with(|search| shelves(all, search))))
        })
    };
    view! {
        <main class="content groups-page public-page">
            <div class="groups-head">
                <h1>"Public catalogues"</h1>
                <a class="button" href=Route::Default.href(&page)>"Back to catalogues"</a>
            </div>
            <Hint text="Catalogues everyone signed in can read. Curators keep them." anchor="public" topic="public catalogues"/>
            <input type="search" class="public-search" placeholder="Search names and descriptions"
                aria-label="Search the public catalogues"
                prop:value=move || search.get() on:input=move |event| search.set(event_target_value(&event))/>
            {error_line(error)}
            {move || shown().map(|(empty, shelves)| match (empty, shelves.is_empty()) {
                (true, _) => view! { <p class="quiet">"No public catalogue yet."</p> }.into_any(),
                (false, true) => view! { <p class="quiet">"No public catalogue matches."</p> }.into_any(),
                (false, false) => shelves.into_iter().map(|shelf| view! { <PublicShelf shelf/> }).collect_view().into_any(),
            })}
        </main>
    }
}

#[component]
fn PublicShelf(shelf: Shelf) -> impl IntoView {
    let label = shelf.label();
    let count = shelf.catalogues.len();
    view! {
        <details class="public-shelf" open>
            <summary><span class="label">{label}</span><small>{count}</small></summary>
            <ul class="plain-list">
                {shelf.catalogues.into_iter().map(|catalogue| view! { <PublicRow catalogue/> }).collect_view()}
            </ul>
        </details>
    }
}

#[component]
fn PublicRow(catalogue: PublicCatalogue) -> impl IntoView {
    let projects = expect_context::<Projects>();
    let project = catalogue.project();
    let day = catalogue.day().map(|day| format!("changed {day}"));
    view! {
        <li>
            <button type="button" class="public-row" on:click=move |_| projects.switch(&project)>
                <span class="name">{catalogue.name.clone()}</span>
                {catalogue.description.clone().map(|text| view! { <span class="description">{text}</span> })}
                <span class="meta">
                    {catalogue.source.clone().map(|source| view! { <span class="source">{source}</span> })}
                    {day.map(|day| view! { <span>{day}</span> })}
                </span>
            </button>
        </li>
    }
}
