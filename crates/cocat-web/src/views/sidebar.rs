use leptos::prelude::*;

use crate::catalogues::{CatalogueItem, catalogue_list};
use crate::edit;
use crate::layout::{Span, reveal};
use crate::route::Route;
use crate::state::Room;
use crate::views::controls::{NameForm, go_to};
use crate::views::submissions::waiting_submissions;
use crate::views::transfer::Importer;

#[component]
pub fn Sidebar(shown: Memo<Route>) -> impl IntoView {
    let room = expect_context::<Room>();
    let reader = room.clone();
    let list = Memo::new(move |_| reader.read(|txn| catalogue_list(txn)));
    let page = window().location().pathname().unwrap_or_default();
    let trash = Route::Trash.href(&page);
    let public = page.clone();
    let entries = NodeRef::<leptos::html::Nav>::new();
    keep_current_in_view(entries, move || shown.track());
    view! {
        <aside class="sidebar" aria-label="Catalogues">
            <div class="sidebar-head">
                <h2 class="eyebrow">"Catalogues"</h2>
                {room.can_write().then(|| view! { <NewCatalogue/> })}
            </div>
            <nav class="catalogue-list" node_ref=entries>
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
            <PublicEntry page=public/>
        </aside>
    }
}

/// The "+" of the list: a new catalogue, with the form it opens, or a file to import.
#[component]
fn NewCatalogue() -> impl IntoView {
    let room = expect_context::<Room>();
    let importer = expect_context::<Importer>();
    let open = RwSignal::new(false);
    let menu = NodeRef::<leptos::html::Details>::new();
    let close = move || {
        if let Some(menu) = menu.get_untracked() {
            menu.set_open(false);
        }
    };
    let writer = room.clone();
    let submit = move |name: String| {
        let room = &writer;
        let author = room.config.user_email.clone();
        let made = room.write(|txn| edit::new_catalogue(txn, &name, &author))?;
        go_to(&Route::Catalogue(made).hash());
        Ok(())
    };
    let submit = StoredValue::new_local(submit);
    let me = room.config.user_email.clone();
    let preset = {
        let room = room.clone();
        move |name: &str, text: &str| {
            let author = room.config.user_email.clone();
            let made = room.write(|txn| {
                let names = cocat_core::model::catalogue_names(txn);
                let predicate = cocat_core::query::parse(text, &names)?;
                let rule = cocat_core::query::rule(&predicate, None);
                edit::new_smart_catalogue(txn, name, &author, &rule)
            });
            if let Ok(made) = made {
                go_to(&Route::Catalogue(made).hash());
            }
        }
    };
    let preset = StoredValue::new_local(preset);
    let presets = crate::search::presets(&me);
    view! {
        <details class="menu-button add-menu" node_ref=menu>
            <summary class="icon add-catalogue" aria-label="Add a catalogue">
                <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                    stroke-width="2" aria-hidden="true"><path d="M12 5v14M5 12h14"></path></svg>
            </summary>
            <div class="menu">
                <button type="button" on:click=move |_| { close(); open.set(true); }>"New catalogue…"</button>
                <button type="button" on:click=move |_| { close(); importer.open(None); }>"Import a file…"</button>
                <p class="menu-group">"New smart catalogue"</p>
                {presets.into_iter().map(|(name, text)| view! {
                    <button type="button" class="preset" title=text.clone()
                        on:click=move |_| { close(); preset.with_value(|preset| preset(name, &text)); }>{name}</button>
                }).collect_view()}
                <p class="menu-hint">"Or search, then “Save as smart catalogue…”."</p>
            </div>
        </details>
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

/// The way to the public catalogues, from any project; a curator also sees how many
/// submissions wait.
#[component]
fn PublicEntry(page: String) -> impl IntoView {
    let room = expect_context::<Room>();
    let waiting = waiting_submissions(room.config.api_url.clone());
    let badge = move || {
        waiting.get().filter(|count| *count > 0).map(|count| {
            let plural = if count == 1 { "" } else { "s" };
            view! {
                <span class="count-badge" title=format!("{count} submission{plural} to review")
                    aria-label=format!("{count} waiting")>{count}</span>
            }
        })
    };
    view! {
        <a
            class="catalogue public-link"
            href=Route::Public(None).href(&page)
        >
            <span class="name">
                <PublicIcon size=14/>
                "Public catalogues"
            </span>
            {badge}
        </a>
    }
}

/// After each change `watch` tracks, scrolls `list`, and nothing around it, so its
/// highlighted entry shows. A list that does not scroll (the phone's stacked layout) stays put.
pub fn keep_current_in_view(list: NodeRef<leptos::html::Nav>, watch: impl Fn() + 'static) {
    Effect::new(move |_| {
        watch();
        if let Some(list) = list.get() {
            request_animation_frame(move || reveal_current(&list));
        }
    });
}

fn reveal_current(list: &web_sys::Element) {
    let Ok(Some(entry)) = list.query_selector("[aria-current]") else {
        return;
    };
    let span = |rect: web_sys::DomRect| Span {
        top: rect.top(),
        height: rect.height(),
    };
    let list_span = Span {
        top: list.get_bounding_client_rect().top(),
        height: f64::from(list.client_height()),
    };
    let entry_span = span(entry.get_bounding_client_rect());
    if let Some(top) = reveal(f64::from(list.scroll_top()), list_span, entry_span) {
        list.set_scroll_top(top.round() as i32);
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

/// A globe: what everyone signed in can read.
#[component]
pub fn PublicIcon(size: u32) -> impl IntoView {
    view! {
        <svg width=size height=size viewBox="0 0 24 24" fill="none" stroke="currentColor"
            stroke-width="2" aria-hidden="true">
            <circle cx="12" cy="12" r="9"></circle>
            <path d="M3 12h18M12 3c2.5 2.6 3.8 5.6 3.8 9s-1.3 6.4-3.8 9c-2.5-2.6-3.8-5.6-3.8-9s1.3-6.4 3.8-9z"></path>
        </svg>
    }
}
