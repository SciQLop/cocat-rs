//! The open catalogue's description, tags and attributes, in the panel above its statistics.

use cocat_core::model::{self, CatalogueSummary};
use leptos::prelude::*;

use crate::catalogues::{attribute_rows, description};
use crate::edit;
use crate::route::Route;
use crate::state::Room;
use crate::views::controls::{commit, error_line};
use crate::views::event_editor::{Attributes, CATALOGUE, Tags, Target};

type Summary = Memo<Option<CatalogueSummary>>;

#[component]
pub fn CatalogueDetails(route: Memo<Route>) -> impl IntoView {
    move || match route.get() {
        Route::Catalogue(uuid) => Some(view! { <Details uuid/> }),
        _ => None,
    }
}

#[component]
fn Details(uuid: String) -> impl IntoView {
    let room = expect_context::<Room>();
    let can_write = room.can_write();
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
    match can_write {
        true => view! {
            <div class="panel-head"><span class="eyebrow">"Catalogue"</span></div>
            <DescriptionField uuid=uuid.clone() summary/>
            <Tags target=Target::new(uuid.clone(), CATALOGUE) tags/>
            <Attributes target=Target::new(uuid, CATALOGUE) attributes/>
        }
        .into_any(),
        false => (move || read_only(tags.get()?, attributes.get()?)).into_any(),
    }
}

/// Commits on change, like the event's comment; a blank one removes it.
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
                <textarea rows="4" placeholder="What the catalogue holds, and where its events come from"
                    prop:value=move || text.get().unwrap_or_default() on:change=save></textarea>
            </label>
            {error_line(error)}
        </section>
    }
}

/// What a reader sees: the tags and attributes, and nothing when there are none. The
/// description is already under the page's title.
fn read_only(tags: Vec<String>, attributes: Vec<(String, String)>) -> Option<impl IntoView> {
    (!tags.is_empty() || !attributes.is_empty()).then(|| {
        view! {
            <div class="panel-head"><span class="eyebrow">"Catalogue"</span></div>
            {(!tags.is_empty()).then(|| view! {
                <div class="tags">
                    {tags.into_iter().map(|tag| view! { <span class="tag">{tag}</span> }).collect_view()}
                </div>
            })}
            {(!attributes.is_empty()).then(|| view! {
                <section>
                    <h3 class="section-label">"Attributes"</h3>
                    <dl class="attributes">
                        {attributes.into_iter().map(|(key, value)| view! {
                            <dt>{key}</dt><dd class="mono">{value}</dd>
                        }).collect_view()}
                    </dl>
                </section>
            })}
        }
    })
}
