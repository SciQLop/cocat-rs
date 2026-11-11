//! A proposer's own proposal, shown in the normal catalogue view: a room of its own, writable
//! while the round is proposing and read-only after.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;
use serde_json::json;

use crate::api;
use crate::edit;
use crate::rounds::{Round, first_catalogue_due, proposal_catalogue_name, proposal_role};
use crate::route::Route;
use crate::state::Room;
use crate::views::live_room::{RoomBrowser, open_room, synced};
use crate::views::transfer::{ImportDialog, Importer};
use cocat_core::model;

/// `POST /api/rooms/{room}/rounds/{round}/proposal`: the caller's proposal document.
#[derive(Deserialize)]
struct MyProposal {
    doc: String,
}

#[component]
pub fn ProposalPage(id: String, rounds: RwSignal<Vec<Round>>) -> impl IntoView {
    let outer = expect_context::<Room>();
    let found = {
        let id = id.clone();
        Memo::new(move |_| {
            rounds.with(|rounds| rounds.iter().find(|round| round.id == id).cloned())
        })
    };
    let opened = RwSignal::new(None::<Result<String, String>>);
    let asked = StoredValue::new(false);
    Effect::new(move |_| {
        if found.with(Option::is_none) || asked.get_value() {
            return;
        }
        asked.set_value(true);
        let (api, project, round) = (
            outer.config.api_url.clone(),
            outer.config.room_id.clone(),
            id.clone(),
        );
        spawn_local(async move {
            let made = api::create::<MyProposal>(
                &api,
                &["rooms", &project, "rounds", &round, "proposal"],
                json!({}),
            )
            .await;
            opened.try_set(Some(
                made.map(|made| made.doc).map_err(|error| error.to_string()),
            ));
        });
    });
    move || match (found.get_untracked(), opened.get()) {
        (Some(round), Some(Ok(doc))) => view! { <ProposalRoom round doc/> }.into_any(),
        (_, Some(Err(why))) => notice_view(&why, true),
        _ => notice_view("Opening your proposal…", false),
    }
}

fn notice_view(text: &str, alert: bool) -> AnyView {
    view! { <main class="content"><p class:quiet=!alert role=alert.then_some("alert")>{text.to_owned()}</p></main> }
        .into_any()
}

/// The proposal's document, live, with the visitor's part in it as its role.
#[component]
fn ProposalRoom(round: Round, doc: String) -> impl IntoView {
    let outer = expect_context::<Room>();
    let Some(config) = outer.config.for_proposal(&doc, proposal_role(&round.state)) else {
        return notice_view("This proposal cannot be opened from this page.", true);
    };
    provide_context(InProposal);
    let importer = Importer::new();
    provide_context(importer);
    let room = match open_room(config) {
        Ok(room) => room,
        Err(error) => return notice_view(&format!("The proposal cannot sync: {error}"), true),
    };
    start_catalogue(
        &room,
        proposal_catalogue_name(&round.name, &room.config.user_email),
    );
    let first = Memo::new(move |_| {
        room.read(|txn| model::catalogues(txn).into_iter().next().map(|c| c.uuid))
    });
    let shown = Memo::new(move |_| Route::Default.or_first(first.get().as_deref()));
    let editable = proposal_role(&round.state).can_write();
    let back =
        Route::Round(round.id.clone()).href(&window().location().pathname().unwrap_or_default());
    // simplify: the role is the round's state when the page opened; a page left open after
    // the round moved on still offers edits, which the server refuses. Upgrade: watch the round.
    let state = if editable {
        "You can edit it until the round moves on."
    } else {
        "Read-only: the round has moved on."
    };
    view! {
        {move || importer.is_open().then(|| view! { <ImportDialog/> })}
        <div class="browser"><div class="pane">
            <RoomBrowser shown>
                <div class="groups-head">
                    <h1>"Your proposal: " {round.name.clone()}</h1>
                    <a class="button" href=back>"Back to the round"</a>
                </div>
                <p class="quiet">{state}</p>
            </RoomBrowser>
        </div></div>
    }
    .into_any()
}

/// Marks the subtree that shows a proposal, so its empty states speak of it.
#[derive(Clone, Copy)]
pub struct InProposal;

/// A first-time proposal is an empty document: once synced, its author gets one catalogue,
/// and never again, even when they delete it.
fn start_catalogue(room: &Room, name: String) {
    let room = room.clone();
    Effect::new(move |_| {
        if synced(&room) && room.can_write() && room.read(|txn| first_catalogue_due(txn)) {
            let author = room.config.user_email.clone();
            _ = room.write(|txn| edit::new_catalogue(txn, &name, &author));
        }
    });
}
