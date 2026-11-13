//! The heading of what is on screen: its name, what kind it is, and a smart catalogue's
//! rule in words or its error.

use std::collections::HashSet;

use cocat_core::model::CatalogueSummary;
use cocat_core::{model, query};
use leptos::prelude::*;

use crate::rounds::decided_in;
use crate::route::Route;
use crate::rows::Row;
use crate::rule_text::{copied_text, counts_text, describe};
use crate::state::Room;
use crate::views::catalogue::Members;

#[derive(Clone, PartialEq)]
pub struct About {
    title: String,
    smart: bool,
    /// A smart catalogue's rule in words, when it reads.
    rule: Option<String>,
    /// Where a copied catalogue came from: its direct source, and the whole chain.
    copied: Option<(String, String)>,
    /// The round (id, name) that decided this catalogue.
    decided_in: Option<(String, String)>,
    /// Its `description` attribute.
    description: Option<String>,
    /// A catalogue's uuid: its heading lists its details; not the trash or every event.
    catalogue: Option<String>,
    /// Its tags and other attributes, shown read-only under the title.
    tags: Vec<String>,
    attributes: Vec<(String, String)>,
}

impl About {
    /// A heading that is only a title.
    pub fn titled(title: &str) -> Self {
        Self {
            title: title.to_owned(),
            smart: false,
            rule: None,
            copied: None,
            decided_in: None,
            description: None,
            catalogue: None,
            tags: Vec::new(),
            attributes: Vec::new(),
        }
    }
}

fn page() -> String {
    window().location().pathname().unwrap_or_default()
}

/// What the heading says of the catalogue `shown`, from the project.
pub fn live_about(room: &Room, shown: Memo<Route>) -> Memo<About> {
    let reader = room.clone();
    Memo::new(move |_| reader.read(|txn| about(txn, &shown.get())))
}

/// The line under the title: what is on screen, and how many events.
pub fn live_counts(
    room: &Room,
    shown: Memo<Route>,
    about: Memo<About>,
    members: Members,
    order: Memo<Vec<usize>>,
    with_search: Signal<bool>,
) -> Signal<Option<String>> {
    let room = room.clone();
    let picked = move |rows: &[Row]| match shown.get() {
        Route::Catalogue(uuid) => {
            let listed: HashSet<String> = room
                .read(|txn| model::listed_events(txn, &uuid))
                .into_iter()
                .collect();
            rows.iter()
                .filter(|row| row.uuid.as_ref().is_some_and(|uuid| listed.contains(uuid)))
                .count()
        }
        _ => 0,
    };
    Signal::derive(move || {
        let (total, picked, public) = members.with(|members| {
            let rows = members.ok()?;
            let public = rows.iter().any(|row| row.catalogue.is_some());
            Some((rows.len(), with_search.get().then(|| picked(rows)), public))
        })?;
        let visible = order.with(Vec::len);
        let smart = about.with(|about| about.smart) && picked.is_none_or(|picked| picked == 0);
        if !matches!(
            shown.get(),
            Route::Catalogue(_) | Route::Trash | Route::AllEvents
        ) {
            return None;
        }
        let (kind, unit) = match (shown.get(), smart) {
            (Route::Trash, _) => ("In the trash", "event"),
            (Route::AllEvents, _) if public => ("In the public catalogues", "event"),
            (Route::AllEvents, _) => ("Every event of the project", "event"),
            (_, true) => ("Smart catalogue", "event"),
            _ => ("Catalogue", "event"),
        };
        Some(counts_text(kind, unit, total, picked, visible))
    })
}

/// Renaming a page's title in place: whether the visitor may, whether the field is open, and
/// what saves a name (the error to show, if it is refused).
#[derive(Clone, Copy)]
pub struct TitleEdit {
    pub allowed: Signal<bool>,
    pub editing: RwSignal<bool>,
    pub save: Callback<String, Result<(), String>>,
}

/// The title as a field: Enter or Save keeps the name, Escape or Cancel leaves it.
#[component]
fn TitleField(about: Memo<About>, rename: TitleEdit) -> impl IntoView {
    let input = NodeRef::<leptos::html::Input>::new();
    let error = RwSignal::new(None::<String>);
    let initial = about.with_untracked(|about| about.title.clone());
    Effect::new(move |_| {
        if let Some(input) = input.get() {
            let _ = input.focus();
            input.select();
        }
    });
    let save = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let name = input.get().map(|input| input.value()).unwrap_or_default();
        match rename.save.run(name) {
            Ok(()) => rename.editing.set(false),
            Err(refused) => error.set(Some(crate::search::sentence(&refused))),
        }
    };
    let escape = move |event: leptos::ev::KeyboardEvent| {
        if event.key() == "Escape" {
            rename.editing.set(false);
        }
    };
    view! {
        <form class="title-field" on:submit=save>
            <input type="text" node_ref=input value=initial aria-label="Name" on:keydown=escape/>
            <button type="submit" class="button primary">"Save"</button>
            <button type="button" class="button" on:click=move |_| rename.editing.set(false)>"Cancel"</button>
        </form>
        {move || error.get().map(|why| view! { <p class="field-error" role="alert">{why}</p> })}
    }
}

/// The heading of the live page and of the preview: `about` and `counts` come from the
/// project or from the server's answers.
#[component]
pub fn Heading(
    about: Memo<About>,
    counts: Signal<Option<String>>,
    members: Members,
    /// Renaming in place, where the visitor may.
    #[prop(optional)]
    rename: Option<TitleEdit>,
) -> impl IntoView {
    let room = expect_context::<Room>();
    let listed = cocat_core::room_id::is_public_id(&room.config.room_id)
        .then(|| crate::views::public::listed(room.config.room_id.clone()));
    let listed = move || listed.and_then(|listed| listed.get());
    let details = use_context::<crate::views::catalogue_details::DetailsEditor>()
        .filter(|_| room.can_write());
    let editing_details = Memo::new(move |_| {
        details
            .filter(|details| details.is_open())
            .and_then(|_| about.with(|about| about.catalogue.clone()))
    });
    view! {
        <div class="heading">
            {move || match rename.filter(|rename| rename.editing.get() && rename.allowed.get()) {
                Some(rename) => view! { <TitleField about rename/> }.into_any(),
                None => view! {
                    <div class="title-line">
                        <h1>{move || about.with(|about| about.title.clone())}</h1>
                        {rename.map(|rename| view! {
                            <Show when=move || rename.allowed.get()>
                                <button type="button" class="rename-pen" title="Rename"
                                    aria-label=move || format!("Rename “{}”", about.with(|about| about.title.clone()))
                                    on:click=move |_| rename.editing.set(true)>
                                    <crate::views::projects::PenIcon/>
                                </button>
                            </Show>
                        })}
                    </div>
                }
                .into_any(),
            }}
            <p class="quiet meta-line">
                {counts}
                {move || about.with(|about| about.copied.clone()).map(|(line, chain)| view! {
                    " · "<span class="copied" title=chain>{line.replacen("Copied from", "copied from", 1)}</span>
                })}
            </p>
            {move || listed().and_then(|listed| listed.relay).map(|relay| view! {
                <p class="quiet">{crate::views::public::relay_line(&relay)}</p>
            })}
            {move || match editing_details.get() {
                // Keyed on the catalogue alone: the fields stay put while their saves change
                // what the heading shows.
                Some(uuid) => view! { <crate::views::catalogue_details::AboutEditor uuid/> }.into_any(),
                None => (move || {
                    let curated = listed().and_then(|listed| listed.description);
                    let description = curated.or_else(|| about.with(|about| about.description.clone()));
                    match about.with(|about| about.catalogue.as_ref().map(|_| (about.tags.clone(), about.attributes.clone()))) {
                        Some((tags, attributes)) => view! {
                            <crate::views::catalogue_details::AboutBox description tags attributes/>
                        }.into_any(),
                        None => description
                            .map(|text| view! { <crate::views::public::Description text/> })
                            .into_any(),
                    }
                }).into_any(),
            }}
            {move || about.with(|about| about.decided_in.clone()).map(|(id, name)| view! {
                <p class="quiet">"Decided in round "<a href=Route::Round(id).href(&page())>{name}</a></p>
            })}
            {move || about.with(|about| about.rule.clone()).map(|words| view! {
                <p class="rule">"Rule: " {words}</p>
            })}
            {move || members.with(|members| members.err().map(str::to_owned)).map(|error| view! {
                <p class="rule-error" role="alert">"The rule does not evaluate: " {error}</p>
            })}
        </div>
    }
}

/// What the heading says of one catalogue; `names` are the project's catalogues, and `name`
/// finds any catalogue's name, trashed or not.
pub fn catalogue_about(
    catalogue: CatalogueSummary,
    names: &[(String, String)],
    name: &dyn Fn(&str) -> Option<String>,
) -> About {
    About {
        title: catalogue.name,
        smart: catalogue.rule.is_some(),
        rule: catalogue
            .rule
            .filter(|rule| query::rule_search(rule, names).is_none())
            .and_then(|rule| describe(&rule, name).ok()),
        copied: copied_text(&catalogue.copied_from),
        decided_in: decided_in(&catalogue.attributes),
        description: crate::catalogues::description(&catalogue.attributes),
        attributes: crate::catalogues::attribute_rows(&catalogue.attributes),
        catalogue: Some(catalogue.uuid),
        tags: catalogue.tags,
    }
}

fn about<T: yrs::ReadTxn>(txn: &T, shown: &Route) -> About {
    let name = |uuid: &str| model::catalogue(txn, uuid).map(|catalogue| catalogue.name);
    let titled = About::titled;
    match shown {
        Route::Catalogue(uuid) => match model::catalogue(txn, uuid) {
            Some(catalogue) => catalogue_about(catalogue, &model::catalogue_names(txn), &name),
            None => titled("No such catalogue"),
        },
        Route::Trash => titled("Trash"),
        Route::AllEvents => titled("All events"),
        _ => titled("No catalogue yet"),
    }
}
