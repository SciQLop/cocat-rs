//! The heading of what is on screen: its name, what kind it is, and a smart catalogue's
//! rule in words or its error.

use cocat_core::model;
use leptos::prelude::*;

use crate::route::Route;
use crate::rule_text::describe;
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
pub fn Heading(shown: Memo<Route>, members: Members, order: Memo<Vec<usize>>) -> impl IntoView {
    let room = expect_context::<Room>();
    let about = Memo::new(move |_| room.read(|txn| about(txn, &shown.get())));
    let counts = move || {
        let total = members.with(|members| members.as_ref().ok().map(Vec::len))?;
        let visible = order.with(Vec::len);
        let (kind, unit) = match (shown.get(), about.with(|about| about.smart)) {
            (Route::Trash, _) => ("In the trash", "event"),
            (_, true) => ("Smart catalogue, recomputed live", "member"),
            _ => ("Catalogue", "event"),
        };
        let plural = if total == 1 { "" } else { "s" };
        let filtered = if visible == total {
            String::new()
        } else {
            format!(" · {visible} shown")
        };
        Some(format!("{kind} · {total} {unit}{plural}{filtered}"))
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
        Route::Default
        | Route::History { .. }
        | Route::Groups
        | Route::Group(_)
        | Route::Public => titled("No catalogue yet"),
    }
}
