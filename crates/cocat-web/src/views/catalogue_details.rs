//! What the catalogue is, apart from what its events measure: its tags and attributes as a
//! block under the page's title, and the editor that takes the side panel's place while an
//! editor changes them.

use cocat_core::model::{self, CatalogueSummary};
use leptos::prelude::*;

use crate::catalogues::{attribute_rows, description, listed_attributes};
use crate::edit;
use crate::route::Route;
use crate::state::Room;
use crate::views::controls::{commit, error_line};
use crate::views::event_editor::{Attributes, CATALOGUE, Tags, Target};

/// Whether the side panel edits the open catalogue's details instead of showing statistics.
#[derive(Clone, Copy)]
pub struct DetailsEditor(pub RwSignal<bool>);

impl DetailsEditor {
    pub fn open(self) {
        self.0.set(true);
    }
}

type Summary = Memo<Option<CatalogueSummary>>;

/// The tags and attributes under the title, read-only: a long attribute list folds to a
/// few, with a button for the rest. An editor gets Edit details.
#[component]
pub fn CatalogueFacts(tags: Vec<String>, attributes: Vec<(String, String)>) -> impl IntoView {
    let editor = use_context::<DetailsEditor>()
        .filter(|_| use_context::<Room>().is_some_and(|room| room.can_write()));
    let open = RwSignal::new(false);
    let count = attributes.len();
    let listed = move || {
        let (shown, _) = listed_attributes(count, open.get());
        attributes[..shown]
            .iter()
            .map(|(key, value)| view! {
                <div class="fact"><dt>{key.clone()}</dt><dd title=value.clone()>{value.clone()}</dd></div>
            })
            .collect_view()
    };
    let toggle = move || match listed_attributes(count, open.get()) {
        (_, Some(total)) => Some(format!("Show all {total}")),
        _ if open.get() => Some("Show fewer".to_owned()),
        _ => None,
    };
    let empty = tags.is_empty() && count == 0;
    view! {
        <div class="catalogue-facts">
            {(!tags.is_empty()).then(|| view! {
                <div class="tags">
                    {tags.into_iter().map(|tag| view! { <span class="tag">{tag}</span> }).collect_view()}
                </div>
            })}
            {(count > 0).then(|| view! { <dl class="facts-grid">{listed}</dl> })}
            <div class="facts-actions">
                {move || toggle().map(|label| view! {
                    <button type="button" class="link-button" aria-expanded=move || open.get().to_string()
                        on:click=move |_| open.update(|open| *open = !*open)>{label}</button>
                })}
                {editor.map(|editor| view! {
                    <button type="button" class="link-button" on:click=move |_| editor.open()>
                        {if empty { "+ Add tags or attributes" } else { "Edit details" }}
                    </button>
                })}
            </div>
        </div>
    }
}

/// The side panel while an editor changes the open catalogue's details; Done brings the
/// statistics back.
#[component]
pub fn CatalogueEditor(route: Memo<Route>) -> impl IntoView {
    let Some(DetailsEditor(editing)) = use_context::<DetailsEditor>() else {
        return ().into_any();
    };
    (move || match route.get() {
        Route::Catalogue(uuid) => Some(view! { <Editor uuid editing/> }),
        _ => None,
    })
    .into_any()
}

#[component]
fn Editor(uuid: String, editing: RwSignal<bool>) -> impl IntoView {
    let room = expect_context::<Room>();
    let summary: Summary = {
        let uuid = uuid.clone();
        Memo::new(move |_| {
            room.read(|txn| model::catalogue(txn, &uuid))
                .filter(|catalogue| !catalogue.deleted)
        })
    };
    let tags = Memo::new(move |_| summary.with(|summary| summary.as_ref().map(|c| c.tags.clone())));
    let attributes = Memo::new(move |_| {
        summary.with(|summary| summary.as_ref().map(|c| attribute_rows(&c.attributes)))
    });
    view! {
        <div class="panel-head editor-head">
            <span class="eyebrow">"Catalogue details"</span>
            <button type="button" class="button primary" on:click=move |_| editing.set(false)>"Done"</button>
        </div>
        <DescriptionField uuid=uuid.clone() summary/>
        <Tags target=Target::new(uuid.clone(), CATALOGUE) tags/>
        <Attributes target=Target::new(uuid, CATALOGUE) attributes/>
    }
}

/// Commits on change, like the event's comment; a blank text removes the description.
#[component]
fn DescriptionField(uuid: String, summary: Summary) -> impl IntoView {
    let room = StoredValue::new_local(expect_context::<Room>());
    let uuid = StoredValue::new_local(uuid);
    let error = RwSignal::new(None);
    let text = Memo::new(move |_| {
        summary.with(|summary| summary.as_ref().and_then(|c| description(&c.attributes)))
    });
    let save = move |event| {
        let typed = event_target_value(&event);
        room.with_value(|room| {
            uuid.with_value(|uuid| {
                commit(room, error, |txn| edit::set_description(txn, uuid, &typed))
            })
        });
    };
    view! {
        <section>
            <label class="field">
                <span class="section-label">"Description"</span>
                <textarea rows="6" placeholder="What the catalogue holds, and where its events come from"
                    prop:value=move || text.get().unwrap_or_default() on:change=save></textarea>
            </label>
            {error_line(error)}
        </section>
    }
}
