//! The decider's page: the reviewed events, the most accepted first, ticked into the final
//! catalogue, which "Decide" writes into the project before the round moves to decided.

use std::collections::HashSet;

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;

use crate::api::{self, ApiError};
use crate::edit::{self, EditError};
use crate::review::{attribute_columns, by_accepts, from_text};
use crate::rounds::{Round, over_cap, state_words};
use crate::route::Route;
use crate::state::Room;
use crate::views::review::{Review, event_cells, find_round, incomplete_view, notice_view};
use crate::views::upload::sleep;
use cocat_core::round::review::{ReviewRow, final_catalogue_uuid, final_events, majority};

#[component]
pub fn DecidePage(id: String, rounds: RwSignal<Vec<Round>>) -> impl IntoView {
    let found = find_round(id, rounds);
    let me = expect_context::<Room>().config.user_email.clone();
    move || match found.get() {
        Some(round) if round.decides_now(&me) => view! { <DecideTable round/> }.into_any(),
        Some(round) => notice_view(&format!(
            "Only a decider builds the final catalogue, while the round is {}.",
            state_words(&round.kind, "voting")
        )),
        None => notice_view("Opening the round…"),
    }
}

#[component]
fn DecideTable(round: Round) -> impl IntoView {
    let review = Review::load(&round);
    let ticked = RwSignal::new(HashSet::<String>::new());
    let failure = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let budget = round.budget_attribute();
    let columns = Memo::new(move |_| {
        review.rows.with(|rows| {
            rows.as_deref()
                .map(|rows| attribute_columns(rows, budget.as_deref()))
                .unwrap_or_default()
        })
    });
    let keep_majority = move |_| {
        review.rows.with_untracked(|rows| {
            if let Some(rows) = rows {
                ticked.set(majority_ticks(rows));
            }
        });
    };
    let round = StoredValue::new(round);
    let decide = move |_| {
        let Some(rows) = review.rows.get_untracked() else {
            return;
        };
        let picked = ticked.with_untracked(|ticked| picks(&rows, ticked));
        failure.set(None);
        match round.with_value(|round| write_final(round, &rows, &picked)) {
            Ok(uuid) => {
                busy.set(true);
                round.with_value(|round| finish(round.id.clone(), uuid, busy, failure));
            }
            Err(error) => failure.set(Some(error.to_string())),
        }
    };
    let back = round.with_value(|round| Route::Round(round.id.clone()).href(&page()));
    let gauge = move || {
        let rows = review.rows.get()?;
        let picked = ticked.with(|ticked| picks(&rows, ticked));
        Some(round.with_value(|round| gauge_view(round, &rows, &picked)))
    };
    view! {
        <div class="groups-head">
            <h1>"Decide: "{round.with_value(|round| round.name.clone())}</h1>
            <a class="button" href=back>"Back to the round"</a>
        </div>
        <p class="quiet">"Tick the events the final catalogue keeps. The most accepted come first."</p>
        <div class="review-bar">
            <button type="button" class="button" on:click=keep_majority>"Keep what a majority accepted"</button>
            {gauge}
            <button type="button" class="button primary" on:click=decide
                disabled=move || busy.get() || review.rows.with(Option::is_none)>
                {move || if busy.get() { "Deciding…" } else { "Decide" }}
            </button>
        </div>
        {move || failure.get().or_else(|| review.failure.get())
            .map(|why| view! { <p class="field-error" role="alert">{why}</p> })}
        {incomplete_view(review)}
        <div class="review-scroll">
            <table class="events review-table">
                <thead><tr>
                    <th>"Keep"</th><th>"Start"</th><th>"Stop"</th><th>"Duration"</th><th>"Confidence"</th>
                    {move || columns.get().into_iter().map(|key| view! { <th>{key}</th> }).collect_view()}
                    <th>"From"</th><th>"Accepts"</th><th>"Rejects"</th><th>"Unsure"</th>
                </tr></thead>
                <tbody>{move || table_rows(review, ticked, columns)}</tbody>
            </table>
        </div>
    }
}

fn page() -> String {
    window().location().pathname().unwrap_or_default()
}

fn majority_ticks(rows: &[ReviewRow]) -> HashSet<String> {
    rows.iter()
        .zip(majority(rows))
        .filter(|(_, kept)| *kept)
        .map(|(row, _)| row.event.uuid.clone())
        .collect()
}

fn picks(rows: &[ReviewRow], ticked: &HashSet<String>) -> Vec<bool> {
    rows.iter()
        .map(|row| ticked.contains(&row.event.uuid))
        .collect()
}

fn gauge_view(round: &Round, rows: &[ReviewRow], picked: &[bool]) -> AnyView {
    let kept: Vec<_> = rows
        .iter()
        .zip(picked)
        .filter(|(_, picked)| **picked)
        .map(|(row, _)| row.event.clone())
        .collect();
    let count = format!("{} events kept", kept.len());
    match round.cost_of(&kept) {
        Some(cost) => {
            let class = if over_cap(&cost) {
                "budget over-cap"
            } else {
                "budget"
            };
            view! { <span class=class>{count}", "{cost.text()}</span> }.into_any()
        }
        None => view! { <span class="budget">{count}</span> }.into_any(),
    }
}

fn table_rows(
    review: Review,
    ticked: RwSignal<HashSet<String>>,
    columns: Memo<Vec<String>>,
) -> Vec<AnyView> {
    let columns = columns.get();
    review.rows.with(|rows| {
        let Some(rows) = rows else {
            return Vec::new();
        };
        by_accepts(rows)
            .into_iter()
            .map(|index| {
                let row = &rows[index];
                let uuid = row.event.uuid.clone();
                let kept = ticked.with(|ticked| ticked.contains(&uuid));
                let toggle = move |_| {
                    ticked.update(|ticked| {
                        if !ticked.remove(&uuid) {
                            ticked.insert(uuid.clone());
                        }
                    })
                };
                let from = from_text(row);
                view! {
                    <tr class="row" aria-selected=kept.to_string()>
                        <td><input type="checkbox" prop:checked=kept on:change=toggle/></td>
                        {event_cells(row, &columns)}
                        <td>{from}</td>
                        <td class="mono">{row.accepts}</td>
                        <td class="mono">{row.rejects}</td>
                        <td class="mono">{row.unsure}</td>
                    </tr>
                }
                .into_any()
            })
            .collect()
    })
}

/// Writes the final catalogue into the project; deciding again after a failure rewrites
/// the same catalogue. Answers its uuid.
fn write_final(round: &Round, rows: &[ReviewRow], picked: &[bool]) -> Result<String, EditError> {
    let room = expect_context::<Room>();
    let uuid = final_catalogue_uuid(&round.id);
    let events = final_events(rows, picked);
    let author = room.config.user_email.clone();
    room.write(|txn| edit::write_final_catalogue(txn, &uuid, &round.name, &author, &events))?;
    Ok(uuid)
}

/// Moves the round to decided with `catalogue`, then opens the catalogue.
fn finish(
    round: String,
    catalogue: String,
    busy: RwSignal<bool>,
    failure: RwSignal<Option<String>>,
) {
    let config = expect_context::<Room>().config;
    let (api, project) = (config.api_url.clone(), config.room_id.clone());
    spawn_local(async move {
        match move_to_decided(&api, &project, &round, &catalogue).await {
            Ok(()) => {
                let _ = window()
                    .location()
                    .set_hash(&Route::Catalogue(catalogue).hash());
            }
            Err(error) => {
                busy.try_set(false);
                failure.try_set(Some(error.to_string()));
            }
        }
    });
}

const TRIES: u32 = 20;
const WAIT_MS: u32 = 250;

// simplify: the page cannot tell when its write reached the server, so it asks again while
// the server does not hold the catalogue yet, for about five seconds. Upgrade: wait for the
// socket's acknowledgement of the update.
async fn move_to_decided(
    api: &str,
    project: &str,
    round: &str,
    catalogue: &str,
) -> Result<(), ApiError> {
    let path = ["rooms", project, "rounds", round, "state"];
    let body = json!({"state": "decided", "final_catalogue": catalogue});
    let mut tries = 1;
    loop {
        match api::post(api, &path, body.clone()).await {
            Err(error) if tries < TRIES && error.to_string().contains("is not in this project") => {
                tries += 1;
                sleep(WAIT_MS).await;
            }
            done => return done,
        }
    }
}
