//! A workspace's page: its files, synced from the manifest, and the file opened.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;

use std::sync::Arc;

use cocat_core::restore::document_json;
use yrs::ReadTxn;
use yrs::types::ToJson;

use crate::api;
use crate::connection;
use crate::format::{minute_text, person_text};
use crate::notebook::notebook;
use crate::projects::api_url;
use crate::route::Route;
use crate::session::RoomConfig;
use crate::state::Room;
use crate::views::header::Header;
use crate::views::notebook::{NotebookView, Place, coloured};
use crate::views::notice::NoticeBar;
use crate::views::projects::Projects;
use crate::workspace::{Entry, Node, Shown, entries, resolve_path, shows_as, size_text, tree};

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

/// The workspace once its file list is known: the header, its files, the file opened.
#[component]
fn Workspace(config: RoomConfig, manifest: String) -> impl IntoView {
    let project_socket = config.ws_url.clone();
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
    let route = expect_context::<Projects>().route;
    let shown = Memo::new(move |_| route.get());
    let reader = manifest_room.clone();
    let files = Memo::new(move |_| reader.read(|txn| manifest_files(txn)));
    let open = Memo::new(move |_| match shown.get() {
        Route::File(path) => Some(path),
        _ => None,
    });
    let config = manifest_room.config.clone();
    let blobs = Blobs {
        api: config.api_url.clone(),
        project: config.room_id.clone(),
    };
    let name = config.room_name.clone();
    view! {
        <Header shown/>
        <NoticeBar/>
        <div class="browser">
            <aside class="sidebar workspace-tree" aria-label="Files">
                <h2 class="eyebrow">"Files"</h2>
                {move || {
                    let listed = files.get();
                    if listed.is_empty() {
                        return view! { <p class="quiet">"No files yet."</p> }.into_any();
                    }
                    tree_view(tree(listed), open)
                }}
            </aside>
            <div class="pane">
                <main class="content file-pane">
                    {move || {
                        let listed = files.get();
                        match open.get() {
                            None if listed.is_empty() => view! {
                                <h1>{name.clone()}</h1>
                                <p class="quiet">"This workspace is empty. Its files appear here once someone shares a SciQLop workspace into it."</p>
                            }.into_any(),
                            None => view! {
                                <h1>{name.clone()}</h1>
                                <p class="quiet">"Pick a file on the left to see it. Files change here as people edit them in SciQLop."</p>
                            }.into_any(),
                            Some(path) => match listed.iter().find(|entry| entry.path == path) {
                                Some(entry) => view! {
                                    <FileView entry=entry.clone() files=listed.clone() socket=project_socket.clone() blobs=blobs.clone()/>
                                }.into_any(),
                                None => view! {
                                    <h1 class="file-title">{path}</h1>
                                    <p class="quiet">"This file was moved or deleted."</p>
                                }.into_any(),
                            },
                        }
                    }}
                </main>
            </div>
        </div>
    }
    .into_any()
}

/// The manifest's `files` map, read as entries.
fn manifest_files<T: ReadTxn>(txn: &T) -> Vec<Entry> {
    let Some(files) = txn.get_map("files") else {
        return Vec::new();
    };
    serde_json::to_value(files.to_json(txn))
        .map(|json| entries(&json))
        .unwrap_or_default()
}

/// Where whole files are stored for this workspace.
#[derive(Clone)]
struct Blobs {
    api: String,
    project: String,
}

impl Blobs {
    fn url(&self, hash: &str) -> Option<String> {
        api_url(&self.api, &["rooms", &self.project, "blobs", hash])
    }
}

fn tree_view(nodes: Vec<Node>, open: Memo<Option<String>>) -> AnyView {
    let page = window().location().pathname().unwrap_or_default();
    view! {
        <ul class="tree">
            {nodes.into_iter().map(|node| match node {
                Node::Folder { name, children, .. } => view! {
                    <li><details open><summary class="folder">{name}</summary>{tree_view(children, open)}</details></li>
                }.into_any(),
                Node::File(entry) => {
                    let href = Route::File(entry.path.clone()).href(&page);
                    let here = entry.path.clone();
                    let name = entry.path.rsplit('/').next().unwrap_or_default().to_owned();
                    let about = about(&entry);
                    view! {
                        <li>
                            <a class="catalogue file-link" href=href title=about
                                aria-current=move || open.with(|open| (open.as_deref() == Some(here.as_str())).then_some("page"))>
                                <span class="name"><span class="label">{name}</span></span>
                                <span class="count">{entry.size.map(size_text)}</span>
                            </a>
                        </li>
                    }.into_any()
                }
            }).collect_view()}
        </ul>
    }
    .into_any()
}

/// "Changed by bob.durand, 2026-11-10 14:02": who last changed the file, and when.
fn about(entry: &Entry) -> String {
    let who = entry.by.as_deref().map(person_text);
    let when = entry.at.map(|at| minute_text(at.saturating_mul(1_000_000)));
    match (who, when) {
        (Some(who), Some(when)) => format!("Changed by {who}, {when}"),
        (Some(who), None) => format!("Changed by {who}"),
        (None, Some(when)) => format!("Changed {when}"),
        (None, None) => String::new(),
    }
}

#[component]
fn FileView(entry: Entry, files: Vec<Entry>, socket: String, blobs: Blobs) -> impl IntoView {
    let title = view! {
        <h1 class="file-title">{entry.path.clone()}</h1>
        <p class="quiet">{about(&entry)}</p>
    };
    let body = match (shows_as(&entry), entry.doc.clone(), entry.hash.clone()) {
        (Shown::Notebook, Some(doc), _) => {
            let place = placing(&entry.path, files, blobs);
            view! { <LiveDocument socket doc render=Render::Notebook(place)/> }.into_any()
        }
        (Shown::Text { language }, Some(doc), _) => {
            view! { <LiveDocument socket doc render=Render::Text(language.unwrap_or("txt"))/> }
                .into_any()
        }
        (Shown::Image, _, Some(hash)) => {
            view! { <img class="file-image" alt=entry.path.clone() src=blobs.url(&hash)/> }
                .into_any()
        }
        (_, _, Some(hash)) => {
            let name = entry.path.rsplit('/').next().unwrap_or_default().to_owned();
            let label = match entry.size {
                Some(size) => format!("Download {name} ({})", size_text(size)),
                None => format!("Download {name}"),
            };
            view! {
                <p class="quiet">"The web app does not show this kind of file."</p>
                <a class="button" href=blobs.url(&hash) download=name>{label}</a>
            }
            .into_any()
        }
        _ => view! { <p class="quiet">"This file has not been shared yet."</p> }.into_any(),
    };
    view! { {title} {body} }
}

/// What a markdown cell's relative image path points at in this workspace, as a stored file.
fn placing(from: &str, files: Vec<Entry>, blobs: Blobs) -> Place {
    let from = from.to_owned();
    Arc::new(move |dest: &str| {
        let path = resolve_path(&from, dest)?;
        let entry = files.iter().find(|entry| entry.path == path)?;
        blobs.url(entry.hash.as_deref()?)
    })
}

#[derive(Clone)]
enum Render {
    Notebook(Place),
    Text(&'static str),
}

/// A notebook or text file's document, synced read-only while it is shown.
#[component]
fn LiveDocument(socket: String, doc: String, render: Render) -> impl IntoView {
    let config = expect_context::<Room>().config.as_ref().clone();
    let room = Room::new(RoomConfig {
        ws_url: format!("{socket}/{doc}"),
        ..config
    });
    let connection = match connection::connect(room.clone()) {
        Ok(connection) => StoredValue::new_local(connection),
        Err(error) => {
            return view! { <p class="field-error" role="alert">{format!("The file cannot sync: {error}")}</p> }
                .into_any();
        }
    };
    on_cleanup(move || {
        connection.try_with_value(|connection| connection.close());
    });
    (move || {
        room.track();
        let json = document_json(&room.doc).unwrap_or_default();
        match render.clone() {
            Render::Notebook(place) => {
                view! { <NotebookView notebook=notebook(&json) place/> }.into_any()
            }
            Render::Text(language) => {
                let text = json["source"].as_str().unwrap_or_default().to_owned();
                view! { <pre class="code text-file"><code>{coloured(&text, language)}</code></pre> }
                    .into_any()
            }
        }
    })
    .into_any()
}
