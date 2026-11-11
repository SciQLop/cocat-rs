//! The selection rounds of the open project, read-only: the list, and one round with who
//! proposed, the log and, once decided, what the final catalogue costs.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::rounds::{Round, over_cap};
use crate::route::Route;
use crate::state::Room;
use cocat_core::model;

fn page() -> String {
    window().location().pathname().unwrap_or_default()
}

/// The project's rounds, read once; empty while loading or when the server refuses.
pub fn project_rounds() -> RwSignal<Vec<Round>> {
    let room = expect_context::<Room>();
    let rounds = RwSignal::new(Vec::new());
    let api = room.config.api_url.clone();
    let id = room.config.room_id.clone();
    spawn_local(async move {
        if let Ok(listed) = api::read::<Vec<Round>>(&api, &["rooms", &id, "rounds"]).await {
            rounds.try_set(listed);
        }
    });
    rounds
}

#[component]
pub fn RoundsPage(shown: Memo<Route>) -> impl IntoView {
    let rounds = project_rounds();
    view! {
        <main class="content groups-page rounds-page">
            {move || match shown.get() {
                Route::Round(id) => round_view(rounds.get(), &id).into_any(),
                _ => round_list(rounds.get()).into_any(),
            }}
        </main>
    }
}

fn round_list(rounds: Vec<Round>) -> impl IntoView {
    let page = page();
    let back = Route::Default.href(&page);
    let rows = rounds
        .into_iter()
        .map(|round| {
            let href = Route::Round(round.id.clone()).href(&page);
            view! {
                <li class="round-row">
                    <a href=href>{round.name}</a>
                    <span class="badge">{round.state}</span>
                </li>
            }
        })
        .collect_view();
    view! {
        <div class="groups-head">
            <h1>"Rounds"</h1>
            <a class="button" href=back>"Back to catalogues"</a>
        </div>
        <ul class="plain-list">{rows}</ul>
    }
}

fn round_view(rounds: Vec<Round>, id: &str) -> impl IntoView {
    let page = page();
    let back = Route::Rounds.href(&page);
    let Some(round) = rounds.into_iter().find(|round| round.id == id) else {
        return view! {
            <div class="groups-head"><h1>"Round"</h1><a class="button" href=back>"Back to rounds"</a></div>
            <p class="quiet">"This round does not exist, or you cannot see it."</p>
        }
        .into_any();
    };
    let settings = settings_line(&round);
    let proposers = round
        .proposers
        .iter()
        .map(|email| view! { <li>{round.proposer_line(email)}</li> })
        .collect_view();
    let deciders = round
        .deciders
        .iter()
        .map(|email| view! { <li>{email.clone()}</li> })
        .collect_view();
    let log = round
        .log
        .iter()
        .map(|entry| {
            let actor = entry.actor.clone().unwrap_or_else(|| "the server".to_owned());
            view! { <li><span class="quiet">{entry.at.clone()}</span>" "{actor}": "{entry.what.clone()}</li> }
        })
        .collect_view();
    view! {
        <div class="groups-head">
            <h1>{round.name.clone()}</h1>
            <a class="button" href=back>"Back to rounds"</a>
        </div>
        <p class="quiet">{settings}</p>
        <section><h2 class="eyebrow">"Proposers"</h2><ul class="plain-list">{proposers}</ul></section>
        <section><h2 class="eyebrow">"Deciders"</h2><ul class="plain-list">{deciders}</ul></section>
        {final_section(&round, &page)}
        <section><h2 class="eyebrow">"Log"</h2><ul class="plain-list">{log}</ul></section>
    }
    .into_any()
}

fn settings_line(round: &Round) -> String {
    let deadline = round
        .deadline
        .as_deref()
        .map(|at| format!(", deadline {at}"))
        .unwrap_or_default();
    format!("{}, {}{deadline}", round.state, round.visibility)
}

fn final_section(round: &Round, page: &str) -> Option<impl IntoView + use<>> {
    let uuid = round.final_catalogue.clone()?;
    let room = expect_context::<Room>();
    let events = room.read(|txn| final_events(txn, &uuid));
    let budget = round.cost_of(&events).map(|cost| {
        let class = if over_cap(&cost) {
            "budget over-cap"
        } else {
            "budget"
        };
        view! { <p class=class>{cost.text()}</p> }
    });
    let href = Route::Catalogue(uuid).href(page);
    Some(view! {
        <section>
            <h2 class="eyebrow">"Final catalogue"</h2>
            <p><a href=href>"Open the final catalogue"</a></p>
            {budget}
        </section>
    })
}

fn final_events<T: yrs::ReadTxn>(txn: &T, uuid: &str) -> Vec<model::EventRecord> {
    let trashed: Vec<String> = model::trash(txn).into_iter().map(|e| e.uuid).collect();
    model::listed_events(txn, uuid)
        .iter()
        .filter(|event| !trashed.contains(event))
        .filter_map(|event| model::event(txn, event))
        .collect()
}
