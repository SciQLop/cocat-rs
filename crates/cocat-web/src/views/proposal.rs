//! A proposer's own proposal, shown in the normal catalogue view: a room of its own, writable
//! while the round is proposing and read-only after.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;
use serde_json::json;

use crate::api;
use crate::connection;
use crate::edit;
use crate::rounds::{Round, proposal_catalogue_name, proposal_role};
use crate::route::Route;
use crate::rows::Row;
use crate::state::Room;
use crate::sync::Status;
use crate::views::catalogue::CataloguePage;
use crate::views::event_panel::{EventPanel, Shown};
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
    let room = Room::new(config);
    provide_context(room.clone());
    let importer = Importer::new();
    provide_context(importer);
    let connection = match connection::connect_project(room.clone()) {
        Ok(connection) => StoredValue::new_local(connection),
        Err(error) => {
            return notice_view(&format!("The proposal cannot sync: {error}"), true);
        }
    };
    on_cleanup(move || {
        connection.try_with_value(|connection| connection.close());
    });
    start_catalogue(
        &room,
        proposal_catalogue_name(&round.name, &room.config.user_email),
    );
    let shown = Memo::new(move |_| Route::Default);
    let selected = RwSignal::new(None::<Row>);
    let screen = RwSignal::new(None);
    let panel = Memo::new(move |_| selected.with(Shown::of));
    let (editor, published) = (room.can_write(), room.editing);
    Effect::new(move |_| {
        let editing = panel.with(|shown| shown.event().map(str::to_owned));
        published.set(editing.filter(|_| editor));
    });
    let back =
        Route::Round(round.id.clone()).href(&window().location().pathname().unwrap_or_default());
    let state = if editor {
        "You can edit it until the round moves on."
    } else {
        "Read-only: the round has moved on."
    };
    view! {
        {move || importer.is_open().then(|| view! { <ImportDialog/> })}
        <main class="content">
            <div class="groups-head">
                <h1>"Your proposal: " {round.name.clone()}</h1>
                <a class="button" href=back>"Back to the round"</a>
            </div>
            <p class="quiet">{state}</p>
            <CataloguePage shown selected screen/>
        </main>
        <EventPanel selected shown=panel route=shown screen/>
    }
    .into_any()
}

/// A first-time proposal is an empty document: once synced, its author gets one catalogue.
fn start_catalogue(room: &Room, name: String) {
    let room = room.clone();
    Effect::new(move |_| {
        let synced = room.status.get() == Status::Live && room.loading.with(Option::is_none);
        if synced && room.can_write() && room.read(|txn| model::catalogues(txn).is_empty()) {
            let author = room.config.user_email.clone();
            _ = room.write(|txn| edit::new_catalogue(txn, &name, &author));
        }
    });
}
