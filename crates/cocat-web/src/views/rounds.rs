//! The selection rounds of the open project, read-only: the list, and one round with who
//! proposed, the log and, once decided, what the final catalogue costs.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::rounds::{
    Round, can_start, group, over_cap, proposal_action, state_words, voter_title, your_part,
};
use crate::route::Route;
use crate::state::Room;
use crate::views::links::is_personal;
use crate::views::projects::Projects;
use crate::views::proposal::ProposalPage;
use crate::views::round_form::NewRoundForm;
use cocat_core::model;
use cocat_core::room_id::is_public_id;

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
    move || match shown.get() {
        Route::Proposal(id) => view! { <ProposalPage id rounds/> }.into_any(),
        route => view! {
            <main class="content groups-page rounds-page">{rounds_view(route, rounds.get())}</main>
        }
        .into_any(),
    }
}

fn rounds_view(route: Route, rounds: Vec<Round>) -> AnyView {
    match route {
        Route::NewRound => view! { <NewRoundForm/> }.into_any(),
        Route::Round(id) => round_view(rounds, &id).into_any(),
        _ => round_list(rounds).into_any(),
    }
}

const INTRO: &str = "In a round, several people propose or review time intervals, then a decider keeps one final catalogue.";

/// Whether the visitor may start a round in the open project.
pub fn can_start_here() -> bool {
    let room = expect_context::<Room>();
    let projects = expect_context::<Projects>();
    let id = &room.config.room_id;
    can_start(
        room.can_write(),
        is_personal(projects, id),
        is_public_id(id),
    )
}

fn me() -> String {
    expect_context::<Room>().config.user_email.clone()
}

fn round_list(rounds: Vec<Round>) -> impl IntoView {
    let page = page();
    let back = Route::Default.href(&page);
    let me = me();
    let (needs_you, in_progress, finished) = group(&rounds, &me);
    let groups = [
        ("Needs you", needs_you),
        ("In progress", in_progress),
        ("Finished", finished),
    ]
    .into_iter()
    .filter(|(_, rounds)| !rounds.is_empty())
    .map(|(title, rounds)| {
        let rows = rounds
            .into_iter()
            .map(|round| round_row(round, &me, &page))
            .collect_view();
        view! { <section><h2 class="eyebrow">{title}</h2><ul class="plain-list">{rows}</ul></section> }
    })
    .collect_view();
    let new_round = can_start_here().then(|| {
        view! { <a class="button primary" href=Route::NewRound.href(&page)>"New round"</a> }
    });
    view! {
        <div class="groups-head">
            <h1>"Rounds"</h1>
            <span class="row-buttons">{new_round}<a class="button" href=back>"Back to catalogues"</a></span>
        </div>
        <p class="quiet">{INTRO}</p>
        {groups}
    }
}

fn round_row(round: &Round, me: &str, page: &str) -> impl IntoView + use<> {
    let href = Route::Round(round.id.clone()).href(page);
    let state = state_words(&round.kind, &round.state).to_owned();
    let part = your_part(round, me, None);
    view! {
        <li class="round-row">
            <a href=href>{round.name.clone()}</a>
            <span class="badge">{round.kind.clone()}</span>
            <span class="badge">{state}</span>
            {part.map(|part| view! { <span class="your-part">{part}</span> })}
        </li>
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
    let proposal = proposal_action(&round, &me()).map(|action| {
        view! { <p><a class="button primary" href=Route::Proposal(round.id.clone()).href(&page)>{action}</a></p> }
    });
    let part = your_part(&round, &me(), None).map(|part| {
        view! { <section class="your-part-box"><h2 class="eyebrow">"Your part"</h2><p>{part}</p>{proposal}</section> }
    });
    let people = |title, emails: Vec<String>| role_section(title, emails);
    let proposers = people(
        "Proposers",
        round
            .proposers
            .iter()
            .map(|email| round.proposer_line(email))
            .collect(),
    );
    let voters = people(voter_title(&round.kind), round.voters.clone());
    let deciders = people("Deciders", round.deciders.clone());
    let log = round
        .log
        .iter()
        .map(|entry| {
            let actor = entry.actor.clone().unwrap_or_else(|| "the server".to_owned());
            let note = entry.note.clone().map(|note| view! { <blockquote class="round-note">{note}</blockquote> });
            view! { <li><span class="quiet">{entry.at.clone()}</span>" "{actor}": "{entry.what.clone()}{note}</li> }
        })
        .collect_view();
    view! {
        <div class="groups-head">
            <h1>{round.name.clone()}</h1>
            <a class="button" href=back>"Back to rounds"</a>
        </div>
        <p class="quiet">{settings}</p>
        {part}
        {proposers}
        {voters}
        {deciders}
        {final_section(&round, &page)}
        <section><h2 class="eyebrow">"Log"</h2><ul class="plain-list">{log}</ul></section>
    }
    .into_any()
}

/// A role's people under its title; nothing when nobody has the role.
fn role_section(title: &'static str, lines: Vec<String>) -> Option<impl IntoView + use<>> {
    (!lines.is_empty()).then(|| {
        let items = lines
            .into_iter()
            .map(|line| view! { <li>{line}</li> })
            .collect_view();
        view! { <section><h2 class="eyebrow">{title}</h2><ul class="plain-list">{items}</ul></section> }
    })
}

fn settings_line(round: &Round) -> String {
    let deadline = round
        .deadline
        .as_deref()
        .map(|at| format!(", deadline {at}"))
        .unwrap_or_default();
    let state = state_words(&round.kind, &round.state);
    format!(
        "{} round, {state}, {}{deadline}",
        round.kind, round.visibility
    )
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
