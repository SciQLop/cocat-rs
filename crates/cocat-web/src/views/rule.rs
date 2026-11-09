//! The heading of what is on screen: its name, what kind it is, and a smart catalogue's
//! rule in words or its error.

use cocat_core::model;
use leptos::prelude::*;

use crate::route::Route;
use crate::rows::rows;
use crate::rule_text::{counts_text, describe};
use crate::state::Room;
use crate::views::catalogue::Members;

#[derive(Clone, PartialEq)]
struct About {
    title: String,
    smart: bool,
    /// A smart catalogue's rule in words, when it reads.
    rule: Option<String>,
}

#[component]
pub fn Heading(
    shown: Memo<Route>,
    members: Members,
    order: Memo<Vec<usize>>,
    everywhere: Signal<bool>,
) -> impl IntoView {
    let room = expect_context::<Room>();
    let reader = room.clone();
    let about = Memo::new(move |_| reader.read(|txn| about(txn, &shown.get())));
    let own_size = move || match shown.get() {
        Route::Catalogue(uuid) => room.read(|txn| rows(txn, &uuid).map_or(0, |rows| rows.len())),
        _ => 0,
    };
    let counts = move || {
        let listed = members.with(|members| members.as_ref().ok().map(Vec::len))?;
        let everywhere = everywhere.get();
        let total = if everywhere { own_size() } else { listed };
        let visible = order.with(Vec::len);
        let (kind, unit) = match (shown.get(), about.with(|about| about.smart)) {
            (Route::Trash, _) => ("In the trash", "event"),
            (Route::AllEvents, _) => ("Every event of the project", "event"),
            (_, true) => ("Smart catalogue, recomputed live", "member"),
            _ => ("Catalogue", "event"),
        };
        Some(counts_text(kind, unit, total, visible, everywhere))
    };
    view! {
        <div class="heading">
            <h1>{move || about.with(|about| about.title.clone())}</h1>
            <p class="quiet">{counts}</p>
            {move || about.with(|about| about.rule.clone()).map(|words| view! {
                <p class="rule">"Rule: " {words}</p>
            })}
            {move || members.with(|members| members.as_ref().err().cloned()).map(|error| view! {
                <p class="rule-error" role="alert">"The rule does not evaluate: " {error}</p>
            })}
        </div>
    }
}

fn about<T: yrs::ReadTxn>(txn: &T, shown: &Route) -> About {
    let name = |uuid: &str| model::catalogue(txn, uuid).map(|catalogue| catalogue.name);
    let titled = |title: &str| About {
        title: title.to_owned(),
        smart: false,
        rule: None,
    };
    match shown {
        Route::Catalogue(uuid) => match model::catalogue(txn, uuid) {
            Some(catalogue) => About {
                title: catalogue.name,
                smart: catalogue.rule.is_some(),
                rule: catalogue.rule.and_then(|rule| describe(&rule, &name).ok()),
            },
            None => titled("No such catalogue"),
        },
        Route::Trash => titled("Trash"),
        Route::AllEvents => titled("All events"),
        _ => titled("No catalogue yet"),
    }
}
