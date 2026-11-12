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
        let (total, picked) = members.with(|members| {
            let rows = members.ok()?;
            Some((rows.len(), with_search.get().then(|| picked(rows))))
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
            (Route::AllEvents, _) => ("Every event of the project", "event"),
            (_, true) => ("Smart catalogue", "event"),
            _ => ("Catalogue", "event"),
        };
        Some(counts_text(kind, unit, total, picked, visible))
    })
}

/// The heading of the live page and of the preview: `about` and `counts` come from the
/// project or from the server's answers.
#[component]
pub fn Heading(
    about: Memo<About>,
    counts: Signal<Option<String>>,
    members: Members,
) -> impl IntoView {
    let room = expect_context::<Room>();
    let listed = cocat_core::room_id::is_public_id(&room.config.room_id)
        .then(|| crate::views::public::listed(room.config.room_id.clone()));
    let listed = move || listed.and_then(|listed| listed.get());
    view! {
        <div class="heading">
            <h1>{move || about.with(|about| about.title.clone())}</h1>
            <p class="quiet">{counts}</p>
            {move || listed().and_then(|listed| listed.relay).map(|relay| view! {
                <p class="quiet">{crate::views::public::relay_line(&relay)}</p>
            })}
            {move || {
                let curated = listed().and_then(|listed| listed.description);
                curated
                    .or_else(|| about.with(|about| about.description.clone()))
                    .map(|text| view! { <crate::views::public::Description text/> })
            }}
            {move || about.with(|about| about.copied.clone()).map(|(line, chain)| view! {
                <p class="quiet copied" title=chain>{line}</p>
            })}
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
