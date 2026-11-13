//! What the catalogue is, apart from what its events measure: its tags and attributes as a
//! block under the page's title, which turns into their fields, in place, while an editor
//! changes them.

use cocat_core::model::{self, CatalogueSummary};
use leptos::prelude::*;

use crate::catalogues::{attribute_rows, description, listed_attributes};
use crate::edit;
use crate::state::Room;
use crate::views::controls::{commit, error_line};
use crate::views::event_editor::{Attributes, CATALOGUE, Tags, Target};

/// Whether the About box under the title edits the open catalogue's details.
#[derive(Clone, Copy)]
pub struct DetailsEditor(pub RwSignal<bool>);

impl DetailsEditor {
    pub fn open(self) {
        self.0.set(true);
    }

    pub fn is_open(self) -> bool {
        self.0.get()
    }
}

type Summary = Memo<Option<CatalogueSummary>>;

/// What the catalogue is, framed under its title: a titled box with its tags, its
/// description and its attributes as a table of names and values, two pairs per row. A long
/// table folds to three rows. An editor gets Edit details in the box's corner, or a link to
/// add details when there are none.
#[component]
pub fn AboutBox(
    description: Option<String>,
    tags: Vec<String>,
    attributes: Vec<(String, String)>,
) -> impl IntoView {
    let editor = use_context::<DetailsEditor>()
        .filter(|_| use_context::<Room>().is_some_and(|room| room.can_write()));
    let empty = description.is_none() && tags.is_empty() && attributes.is_empty();
    if empty {
        return editor
            .map(|editor| view! {
                <button type="button" class="link-button add-link about-add" on:click=move |_| editor.open()>
                    "+ Add a description or attributes"
                </button>
            })
            .into_any();
    }
    let open = RwSignal::new(false);
    let count = attributes.len();
    let rows = move || {
        let (shown, _) = listed_attributes(count, open.get());
        attributes[..shown]
            .iter()
            .map(|(key, value)| {
                view! {
                    <dt>{key.clone()}</dt><dd title=value.clone()>{value.clone()}</dd>
                }
            })
            .collect_view()
    };
    let toggle = move || match listed_attributes(count, open.get()) {
        (_, Some(more)) => Some(format!("+ {more} more ▾")),
        _ if open.get() => Some("Show fewer ▴".to_owned()),
        _ => None,
    };
    view! {
        <section class="about-box" aria-labelledby="about-title">
            <header class="about-head">
                <h2 id="about-title" class="eyebrow">"About this catalogue"</h2>
                {(!tags.is_empty()).then(|| view! {
                    <div class="tags">
                        {tags.into_iter().map(|tag| view! { <span class="tag">{tag}</span> }).collect_view()}
                    </div>
                })}
                {editor.map(|editor| view! {
                    <button type="button" class="link-button about-edit" on:click=move |_| editor.open()>"Edit details"</button>
                })}
            </header>
            {description.map(|text| view! { <crate::views::public::Description text/> })}
            {(count > 0).then(|| view! {
                <dl class="about-attributes" aria-label="Attributes">{rows}</dl>
                {move || toggle().map(|label| view! {
                    <button type="button" class="link-button about-more" aria-expanded=move || open.get().to_string()
                        on:click=move |_| open.update(|open| *open = !*open)>{label}</button>
                })}
            })}
        </section>
    }
    .into_any()
}

/// The About box as fields, where it shows them: each change is saved as it is made, and
/// Done shows the box again.
#[component]
pub fn AboutEditor(uuid: String) -> impl IntoView {
    let room = expect_context::<Room>();
    let editing = use_context::<DetailsEditor>();
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
        <section class="about-box about-editing" aria-labelledby="about-title">
            <header class="about-head">
                <h2 id="about-title" class="eyebrow">"About this catalogue"</h2>
                <button type="button" class="button primary about-done"
                    on:click=move |_| if let Some(DetailsEditor(open)) = editing { open.set(false) }>"Done"</button>
            </header>
            <DescriptionField uuid=uuid.clone() summary/>
            <Tags target=Target::new(uuid.clone(), CATALOGUE) tags/>
            <Attributes target=Target::new(uuid, CATALOGUE) attributes/>
        </section>
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
