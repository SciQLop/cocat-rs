//! A round's review table: the candidates and every proposal the visitor may read, one row
//! per event, one verdict per key press.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{Value, json};
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{Element, KeyboardEvent, ScrollIntoViewOptions, ScrollLogicalPosition};

use crate::api;
use crate::format::{duration_text, time_text, value_text};
use crate::review::{
    Filter, INCOMPLETE, KeyAction, SYNC_WAIT_MS, after_verdict, arrived, attribute_columns,
    counter, from_text, key_action, mine_text, selected, shown as shown_rows, step,
};
use crate::rounds::Round;
use crate::route::Route;
use crate::state::Room;
use crate::views::live_room::{connect_room, synced};
use crate::views::projects::Projects;
use crate::views::upload::sleep;
use cocat_core::role::Role;
use cocat_core::round::review::{ReviewRow, Verdict, progress, proposal_events, rows};
use cocat_core::time::Timestamp;

/// The review rows, none until everything arrived.
pub type ReviewRows = Memo<Option<Vec<ReviewRow>>>;

/// A round's review rows: its documents live, the verdicts as last fetched; none until
/// every document synced and the verdicts arrived.
#[derive(Clone, Copy)]
pub struct Review {
    pub rows: ReviewRows,
    pub failure: RwSignal<Option<String>>,
    /// Some proposal documents did not sync in time, so `rows` leaves their events out.
    pub incomplete: Memo<bool>,
    verdicts: RwSignal<Option<Value>>,
    /// The latest verdicts fetch: an older answer arriving after it is dropped.
    asked: StoredValue<u64>,
    /// The API, the project and the round.
    path: StoredValue<[String; 3]>,
}

impl Review {
    pub fn load(round: &Round) -> Self {
        let outer = expect_context::<Room>();
        let me = outer.config.user_email.clone();
        let docs: Vec<(Option<String>, Room)> = round
            .review_docs()
            .into_iter()
            .filter_map(|(author, doc)| {
                let config = outer.config.for_proposal(&doc, Role::Viewer)?;
                Some((author, connect_room(config).ok()?))
            })
            .collect();
        let verdicts = RwSignal::new(None::<Value>);
        let waited_out = RwSignal::new(false);
        spawn_local(async move {
            sleep(SYNC_WAIT_MS).await;
            waited_out.try_set(true);
        });
        let docs = StoredValue::new(docs);
        let loaded = Memo::new(move |_| {
            let events = docs.with_value(|docs| {
                docs.iter()
                    .map(|(author, room)| {
                        synced(room)
                            .then(|| (author.clone(), room.read(|txn| proposal_events(txn))))
                    })
                    .collect()
            });
            arrived(events, waited_out.get())
        });
        let rows = Memo::new(move |_| {
            let verdicts = verdicts.get()?;
            let (events, _) = loaded.get()?;
            rows(&events, &verdicts, &me).ok()
        });
        let incomplete =
            Memo::new(move |_| loaded.with(|loaded| matches!(loaded, Some((_, true)))));
        let path = [
            outer.config.api_url.clone(),
            outer.config.room_id.clone(),
            round.id.clone(),
        ];
        let review = Self {
            rows,
            incomplete,
            failure: RwSignal::new(None),
            verdicts,
            asked: StoredValue::new(0),
            path: StoredValue::new(path),
        };
        review.refresh();
        review
    }

    pub fn refresh(self) {
        let ticket = self.asked.get_value() + 1;
        self.asked.set_value(ticket);
        let [api, project, round] = self.path.get_value();
        spawn_local(async move {
            let path = ["rooms", &project, "rounds", &round, "verdicts"];
            let answer = api::read::<Value>(&api, &path).await;
            if self.asked.try_get_value() != Some(ticket) {
                return;
            }
            match answer {
                Ok(verdicts) => _ = self.verdicts.try_set(Some(verdicts)),
                Err(error) => _ = self.failure.try_set(Some(error.to_string())),
            }
        });
    }

    /// Sends the visitor's verdict on `event`, then fetches the verdicts again.
    pub fn give(self, event: String, verdict: Verdict) {
        let [api, project, round] = self.path.get_value();
        spawn_local(async move {
            let path = ["rooms", &project, "rounds", &round, "verdicts", &event];
            match api::put(&api, &path, json!({ "verdict": verdict })).await {
                Ok(()) => self.refresh(),
                Err(error) => {
                    self.failure.try_set(Some(error.to_string()));
                }
            }
        });
    }
}

/// The round `id` once the list has it.
pub fn find_round(id: String, rounds: RwSignal<Vec<Round>>) -> Memo<Option<Round>> {
    Memo::new(move |_| rounds.with(|rounds| rounds.iter().find(|round| round.id == id).cloned()))
}

/// The warning shown when some proposals did not load.
pub fn incomplete_view(review: Review) -> impl IntoView {
    move || {
        review
            .incomplete
            .get()
            .then(|| view! { <p class="field-error" role="alert">{INCOMPLETE}</p> })
    }
}

pub fn notice_view(text: &str) -> AnyView {
    view! { <p class="quiet">{text.to_owned()}</p> }.into_any()
}

#[component]
pub fn ReviewPage(id: String, rounds: RwSignal<Vec<Round>>, shown: Memo<Route>) -> impl IntoView {
    let found = find_round(id, rounds);
    move || match found.get() {
        Some(round) => view! { <ReviewTable round shown/> }.into_any(),
        None => notice_view("Opening the round…"),
    }
}

#[component]
fn ReviewTable(round: Round, shown: Memo<Route>) -> impl IntoView {
    let review = Review::load(&round);
    let filter = RwSignal::new(Filter::All);
    let named = Memo::new(move |_| match shown.get() {
        Route::Review { row, .. } => row,
        _ => None,
    });
    let visible = Memo::new(move |_| {
        review.rows.with(|rows| {
            rows.as_deref()
                .map(|rows| shown_rows(rows, filter.get()))
                .unwrap_or_default()
        })
    });
    let current = Memo::new(move |_| {
        review.rows.with(|rows| {
            let rows = rows.as_deref()?;
            visible.with(|visible| selected(rows, visible, named.get().as_deref()))
        })
    });
    let pick = picker(round.id.clone(), review);
    let me = expect_context::<Room>().config.user_email.clone();
    let reviewer = round.reviews_now(&me);
    if reviewer {
        follow_keys(review, visible, current, pick);
    }
    let how = match (reviewer, round.decides_now(&me)) {
        (true, _) => "Keys: a accept, r reject, u unsure, j next, k previous",
        (false, true) => "You decide this round; reviewers give the verdicts.",
        _ => "Read-only: reviewers give the verdicts while the round is reviewing.",
    };
    Effect::new(move |_| {
        let uuid = current.get().and_then(|index| uuid_at(review, index));
        uuid.map(|uuid| bring_into_view(&uuid))
    });
    let budget = round.budget_attribute();
    let columns = Memo::new(move |_| {
        review.rows.with(|rows| {
            rows.as_deref()
                .map(|rows| attribute_columns(rows, budget.as_deref()))
                .unwrap_or_default()
        })
    });
    let back = Route::Round(round.id.clone()).href(&page());
    view! {
        <div class="groups-head">
            <h1>"Review: "{round.name.clone()}</h1>
            <a class="button" href=back>"Back to the round"</a>
        </div>
        <div class="review-bar">
            <strong class="review-counter">{move || review.rows.with(|rows| match rows {
                Some(rows) => counter(rows),
                None => "Loading the events…".to_owned(),
            })}</strong>
            <label class="inline-choice">"Show "
                <select on:change=move |event| filter.set(filter_of(&event_target_value(&event)))>
                    <option value="all">"All"</option>
                    <option value="not-reviewed">"Not reviewed"</option>
                </select>
            </label>
            <span class="hint">{how}</span>
        </div>
        {move || review.failure.get().map(|why| view! { <p class="field-error" role="alert">{why}</p> })}
        {incomplete_view(review)}
        <div class="review-scroll">
            <table class="events review-table">
                <thead><tr>
                    <th>"Start"</th><th>"Stop"</th><th>"Duration"</th><th>"Confidence"</th>
                    {move || columns.get().into_iter().map(|key| view! { <th>{key}</th> }).collect_view()}
                    <th>"From"</th><th>"Accepts"</th><th>"Rejects"</th><th>"Unsure"</th><th>"Mine"</th>
                </tr></thead>
                <tbody>{move || table_rows(review, visible, current, columns, pick)}</tbody>
            </table>
        </div>
        {move || set_aside_note(review)}
    }
}

fn filter_of(value: &str) -> Filter {
    if value == "not-reviewed" {
        Filter::NotReviewed
    } else {
        Filter::All
    }
}

fn page() -> String {
    window().location().pathname().unwrap_or_default()
}

fn uuid_at(review: Review, index: usize) -> Option<String> {
    review
        .rows
        .with_untracked(|rows| Some(rows.as_ref()?.get(index)?.event.uuid.clone()))
}

/// Picks a row: the URL names it, in place, so a reload or a shared link opens on it.
fn picker(id: String, review: Review) -> impl Fn(usize) + Copy + 'static {
    let projects = expect_context::<Projects>();
    let id = StoredValue::new(id);
    move |index| {
        let Some(row) = uuid_at(review, index) else {
            return;
        };
        let route = Route::Review {
            id: id.get_value(),
            row: Some(row),
        };
        if let Ok(history) = window().history() {
            let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&route.href(&page())));
        }
        projects.follow_hash();
    }
}

/// The review keys, while the page is open.
fn follow_keys(
    review: Review,
    visible: Memo<Vec<usize>>,
    current: Memo<Option<usize>>,
    pick: impl Fn(usize) + Copy + 'static,
) {
    let listener = window_event_listener(ev::keydown, move |event: KeyboardEvent| {
        if event.ctrl_key() || event.meta_key() || event.alt_key() || typing(&event) {
            return;
        }
        let (Some(action), Some(index)) = (key_action(&event.key()), current.get_untracked())
        else {
            return;
        };
        event.prevent_default();
        let next = match action {
            KeyAction::Verdict(verdict) => give(review, index, verdict),
            KeyAction::Next => visible.with_untracked(|shown| step(shown, Some(index), true)),
            KeyAction::Previous => visible.with_untracked(|shown| step(shown, Some(index), false)),
        };
        if let Some(next) = next {
            pick(next);
        }
    });
    on_cleanup(move || listener.remove());
}

/// Gives the verdict on row `index`; answers the row to go to.
fn give(review: Review, index: usize, verdict: Verdict) -> Option<usize> {
    let (uuid, next) = review.rows.with_untracked(|rows| {
        let rows = rows.as_deref()?;
        Some((
            rows.get(index)?.event.uuid.clone(),
            after_verdict(rows, index),
        ))
    })?;
    review.give(uuid, verdict);
    Some(next)
}

fn typing(event: &KeyboardEvent) -> bool {
    event
        .target()
        .and_then(|target| target.dyn_into::<Element>().ok())
        .is_some_and(|element| {
            matches!(element.tag_name().as_str(), "INPUT" | "SELECT" | "TEXTAREA")
        })
}

fn row_id(uuid: &str) -> String {
    format!("review-{uuid}")
}

fn bring_into_view(uuid: &str) {
    if let Some(element) = document().get_element_by_id(&row_id(uuid)) {
        let options = ScrollIntoViewOptions::new();
        options.set_block(ScrollLogicalPosition::Nearest);
        element.scroll_into_view_with_scroll_into_view_options(&options);
    }
}

fn table_rows(
    review: Review,
    visible: Memo<Vec<usize>>,
    current: Memo<Option<usize>>,
    columns: Memo<Vec<String>>,
    pick: impl Fn(usize) + Copy + 'static,
) -> Vec<AnyView> {
    let picked = current.get();
    let columns = columns.get();
    review.rows.with(|rows| {
        let Some(rows) = rows else {
            return Vec::new();
        };
        visible
            .get()
            .into_iter()
            .map(|index| {
                let row = &rows[index];
                let cells = event_cells(row, &columns);
                let from = from_text(row);
                view! {
                    <tr class="row" id=row_id(&row.event.uuid)
                        aria-selected=(picked == Some(index)).to_string()
                        on:click=move |_| pick(index)>
                        {cells}
                        <td>{from}</td>
                        <td class="mono">{row.accepts}</td>
                        <td class="mono">{row.rejects}</td>
                        <td class="mono">{row.unsure}</td>
                        <td class="mine">{mine_text(row)}</td>
                    </tr>
                }
                .into_any()
            })
            .collect()
    })
}

fn nanos(at: Timestamp) -> i64 {
    at.unix_nanos().unwrap_or_default()
}

/// The event's own cells: its times, its confidence and the attribute columns.
pub fn event_cells(row: &ReviewRow, columns: &[String]) -> impl IntoView + use<> {
    let event = &row.event;
    let (start, stop) = (nanos(event.start), nanos(event.stop));
    let confidence = event
        .confidence
        .map(|c| format!("{c:.2}"))
        .unwrap_or_default();
    let attributes = columns
        .iter()
        .map(|key| {
            let text = event
                .attributes
                .get(key)
                .map(value_text)
                .unwrap_or_default();
            view! { <td>{text}</td> }
        })
        .collect_view();
    view! {
        <td class="mono">{time_text(start)}</td>
        <td class="mono">{time_text(stop)}</td>
        <td class="mono">{duration_text(stop - start)}</td>
        <td class="mono">{confidence}</td>
        {attributes}
    }
}

fn set_aside_note(review: Review) -> Option<impl IntoView> {
    let (_, _, set_aside) = review.rows.with(|rows| rows.as_deref().map(progress))?;
    (set_aside > 0).then(|| {
        view! { <p class="quiet">{format!("{set_aside} verdicts on events no longer under review are set aside.")}</p> }
    })
}
