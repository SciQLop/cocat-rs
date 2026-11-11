//! The selection rounds of the open project, read-only: the list, and one round with who
//! proposed, the log and, once decided, what the final catalogue costs.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::rounds::{
    Progress, Round, can_start, group, over_cap, proposal_action, state_words, voter_title,
    your_part,
};
use crate::route::Route;
use crate::state::Room;
use crate::views::decide::DecidePage;
use crate::views::links::is_personal;
use crate::views::projects::Projects;
use crate::views::proposal::ProposalPage;
use crate::views::review::{Review, ReviewPage, ReviewRows, find_round};
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
    let on = Memo::new(move |_| shown.with(Route::page));
    move || {
        match on.get() {
        Route::Proposal(id) => view! { <ProposalPage id rounds/> }.into_any(),
        Route::Review { id, .. } => view! {
            <main class="content groups-page rounds-page review-page"><ReviewPage id rounds shown/></main>
        }
        .into_any(),
        Route::Decide(id) => view! {
            <main class="content groups-page rounds-page review-page"><DecidePage id rounds/></main>
        }
        .into_any(),
        Route::Round(id) => view! {
            <main class="content groups-page rounds-page"><RoundPage id rounds/></main>
        }
        .into_any(),
        Route::NewRound => view! {
            <main class="content groups-page rounds-page"><NewRoundForm/></main>
        }
        .into_any(),
        _ => view! { <main class="content groups-page rounds-page">{move || round_list(rounds.get())}</main> }
            .into_any(),
    }
    }
}

/// One round. Its review documents open once, when the visitor reviews or decides it now,
/// not again on each reload of the list.
#[component]
fn RoundPage(id: String, rounds: RwSignal<Vec<Round>>) -> impl IntoView {
    let found = find_round(id, rounds);
    let me = me();
    let reviewing = Memo::new(move |_| {
        found.with(|round| {
            round
                .as_ref()
                .map(|round| round.reviews_now(&me) || round.decides_now(&me))
        })
    });
    move || match reviewing.get() {
        None => missing_round().into_any(),
        Some(reviewing) => {
            let rows = reviewing
                .then(|| found.with_untracked(|round| round.as_ref().map(|r| Review::load(r).rows)))
                .flatten();
            (move || found.get().map(|round| round_view(round, rows))).into_any()
        }
    }
}

fn missing_round() -> impl IntoView {
    let back = Route::Rounds.href(&page());
    view! {
        <div class="groups-head"><h1>"Round"</h1><a class="button" href=back>"Back to rounds"</a></div>
        <p class="quiet">"This round does not exist, or you cannot see it."</p>
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

fn round_view(round: Round, rows: Option<ReviewRows>) -> impl IntoView {
    let page = page();
    let back = Route::Rounds.href(&page);
    let settings = settings_line(&round);
    let part = view! { <YourPart round=round.clone() rows/> };
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
}

/// What the round asks of the visitor, with the counts once the events and verdicts are in,
/// and the buttons to do it.
#[component]
fn YourPart(round: Round, rows: Option<ReviewRows>) -> impl IntoView {
    let (me, page) = (me(), page());
    let part = your_part(&round, &me, None)?;
    let link = |label: &'static str, route: Route| {
        view! { <a class="button primary" href=route.href(&page)>{label}</a> }
    };
    let id = round.id.clone();
    let proposal =
        proposal_action(&round, &me).map(|action| link(action, Route::Proposal(id.clone())));
    let review = round.reviews_now(&me).then(|| {
        link(
            "Review the events",
            Route::Review {
                id: id.clone(),
                row: None,
            },
        )
    });
    let decide = round
        .decides_now(&me)
        .then(|| link("Build the final catalogue", Route::Decide(id.clone())));
    let text = move || {
        let progress = rows.and_then(|rows| rows.with(|rows| rows.as_deref().map(Progress::of)));
        your_part(&round, &me, progress).unwrap_or_else(|| part.clone())
    };
    Some(view! {
        <section class="your-part-box">
            <h2 class="eyebrow">"Your part"</h2>
            <p>{text}</p>
            <p class="row-buttons">{proposal}{review}{decide}</p>
        </section>
    })
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
