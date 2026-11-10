//! A workspace's page: its files, synced from the manifest, and the file opened.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;

use crate::api;
use crate::connection;
use crate::session::RoomConfig;
use crate::state::Room;
use crate::views::header::Header;
use crate::views::notice::NoticeBar;

/// One document as `GET /api/rooms/{id}/documents` lists it.
#[derive(Debug, Clone, Deserialize)]
struct Document {
    id: String,
    kind: String,
}

/// What the page knows of the workspace's documents.
#[derive(Debug, Clone)]
enum Documents {
    Loading,
    Failed(String),
    Listed(Vec<Document>),
}

#[component]
pub fn WorkspaceApp(config: RoomConfig) -> impl IntoView {
    let documents = RwSignal::new(Documents::Loading);
    let (api, id) = (config.api_url.clone(), config.room_id.clone());
    spawn_local(async move {
        let listed = api::read::<Vec<Document>>(&api, &["rooms", &id, "documents"]).await;
        documents.try_set(match listed {
            Ok(listed) => Documents::Listed(listed),
            Err(failure) => Documents::Failed(failure.to_string()),
        });
    });
    move || {
        match documents.get() {
        Documents::Loading => view! { <p class="quiet page-note">"Opening the workspace…"</p> }.into_any(),
        Documents::Failed(why) => view! { <p class="field-error page-note" role="alert">{why}</p> }.into_any(),
        Documents::Listed(listed) => match manifest_of(&listed) {
            Some(manifest) => view! { <Workspace config=config.clone() manifest/> }.into_any(),
            None => view! { <p class="field-error page-note" role="alert">"This workspace has no file list."</p> }.into_any(),
        },
    }
    }
}

fn manifest_of(documents: &[Document]) -> Option<String> {
    documents
        .iter()
        .find(|document| document.kind == "manifest")
        .map(|document| document.id.clone())
}

/// The workspace once its file list is known: the header, and its files.
#[component]
fn Workspace(config: RoomConfig, manifest: String) -> impl IntoView {
    let manifest_room = Room::new(RoomConfig {
        ws_url: format!("{}/{manifest}", config.ws_url),
        ..config
    });
    provide_context(manifest_room.clone());
    let connection = match connection::connect(manifest_room.clone()) {
        Ok(connection) => StoredValue::new_local(connection),
        Err(error) => {
            return view! { <p class="field-error page-note" role="alert">{format!("The workspace cannot sync: {error}")}</p> }
                .into_any();
        }
    };
    on_cleanup(move || {
        connection.try_with_value(|connection| connection.close());
    });
    let shown = Memo::new(move |_| crate::route::Route::Default);
    view! {
        <Header shown/>
        <NoticeBar/>
        <div class="browser">
            <main class="content workspace-empty">
                <h1>{manifest_room.config.room_name.clone()}</h1>
                <p class="quiet">"This workspace is empty. Its files appear here once someone shares a SciQLop workspace into it."</p>
            </main>
        </div>
    }
    .into_any()
}
