//! A project's files: its file list synced from the manifest, the Files section of the
//! sidebar, and the file opened in the main pane.

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
use crate::history::Version;
use crate::notebook::notebook;
use crate::projects::api_url;
use crate::route::Route;
use crate::session::RoomConfig;
use crate::state::Room;
use crate::sync::Status;
use crate::views::controls::{Error, error_line};
use crate::views::notebook::{NotebookView, Place, coloured};
use crate::workspace::{
    Entry, Node, Shown, entries, is_image, placed_image, same_view, shows_as, size_text, tree,
    versions_that_changed,
};

/// One document as `GET /api/rooms/{id}/documents` lists it.
#[derive(Debug, Clone, Deserialize)]
struct Document {
    id: String,
    kind: String,
}

/// The project's files once its file list is synced: `None` while it loads, or when the
/// project has none. Provided by the project page; the sidebar and the file pane read it.
pub type ProjectFiles = RwSignal<Option<Files>>;

#[derive(Clone)]
pub struct Files {
    files: Memo<Vec<Entry>>,
    socket: String,
    blobs: Blobs,
    manifest: String,
}

/// Finds the project's file list and keeps it synced while the page shows the project.
#[component]
pub fn FilesLoader(config: RoomConfig, state: ProjectFiles) -> impl IntoView {
    let manifest = RwSignal::new(None::<String>);
    let failure = RwSignal::new(None::<String>);
    let (api, id) = (config.api_url.clone(), config.room_id.clone());
    spawn_local(async move {
        match api::read::<Vec<Document>>(&api, &["rooms", &id, "documents"]).await {
            Ok(listed) => {
                manifest.try_set(manifest_of(&listed));
            }
            Err(error) => {
                failure.try_set(Some(error.to_string()));
            }
        }
    });
    view! {
        {move || failure.get().map(|why| view! { <p class="field-error page-note" role="alert">{format!("The files cannot be read: {why}")}</p> })}
        {move || manifest.get().map(|manifest| view! { <ManifestSync config=config.clone() manifest state/> })}
    }
}

fn manifest_of(documents: &[Document]) -> Option<String> {
    documents
        .iter()
        .find(|document| document.kind == "manifest")
        .map(|document| document.id.clone())
}

/// Syncs the file list as a read replica (not the page's catalogues room).
#[component]
fn ManifestSync(config: RoomConfig, manifest: String, state: ProjectFiles) -> impl IntoView {
    let socket = config.ws_url.clone();
    let Some(ws_url) = api_url(&config.ws_url, &[&manifest]) else {
        return view! { <p class="field-error page-note" role="alert">"The files cannot sync."</p> }.into_any();
    };
    let blobs = Blobs {
        api: config.api_url.clone(),
        project: config.room_id.clone(),
    };
    let room = Room::new(RoomConfig { ws_url, ..config });
    let connection = match connection::connect(room.clone()) {
        Ok(connection) => StoredValue::new_local(connection),
        Err(error) => {
            return view! { <p class="field-error page-note" role="alert">{format!("The files cannot sync: {error}")}</p> }
                .into_any();
        }
    };
    on_cleanup(move || {
        connection.try_with_value(|connection| connection.close());
        state.try_set(None);
    });
    let files = Memo::new(move |_| room.read(|txn| manifest_files(txn)));
    state.set(Some(Files {
        files,
        socket,
        blobs,
        manifest,
    }));
    ().into_any()
}

/// The sidebar's Files section: the project's files as a tree.
#[component]
pub fn FilesSection(state: ProjectFiles, shown: Memo<Route>) -> impl IntoView {
    let open = Memo::new(move |_| match shown.get() {
        Route::File(path) => Some(path),
        _ => None,
    });
    move || {
        state.get().map(|project| {
            view! {
                <section class="files-section" aria-label="Files">
                    <h2 class="eyebrow">"Files"</h2>
                    {move || {
                        let listed = project.files.get();
                        if listed.is_empty() {
                            return view! { <p class="quiet">"No files yet."</p> }.into_any();
                        }
                        tree_view(tree(listed), open)
                    }}
                </section>
            }
        })
    }
}

/// The main pane on a file's route: the file, or why it is not there.
#[component]
pub fn FilePane(state: ProjectFiles, path: String) -> impl IntoView {
    move || {
        let Some(project) = state.get() else {
            return view! { <p class="quiet">"Opening the files…"</p> }.into_any();
        };
        let files = project.files;
        let wanted = path.clone();
        let opened = Memo::new_with_compare(
            move |_| files.with(|files| files.iter().find(|entry| entry.path == wanted).cloned()),
            |old, new| !same_view(old.and_then(Option::as_ref), new.and_then(Option::as_ref)),
        );
        let path = path.clone();
        (move || match opened.get() {
            Some(entry) => view! {
                <FileView entry files socket=project.socket.clone() blobs=project.blobs.clone() manifest=project.manifest.clone()/>
            }.into_any(),
            None => view! {
                <h1 class="file-title">{path.clone()}</h1>
                <p class="quiet">"This file was moved or deleted."</p>
            }.into_any(),
        })
        .into_any()
    }
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
fn FileView(
    entry: Entry,
    files: Memo<Vec<Entry>>,
    socket: String,
    blobs: Blobs,
    manifest: String,
) -> impl IntoView {
    let in_history = RwSignal::new(false);
    let about_line = {
        let path = entry.path.clone();
        move || files.with(|files| files.iter().find(|e| e.path == path).map(about))
    };
    let shown = shows_as(&entry);
    let place = placing(&entry.path, files, blobs.clone());
    let history_entry = entry.clone();
    let history_blobs = blobs.clone();
    let history_place = place.clone();
    let live = move || {
        match (shown, entry.doc.clone(), entry.hash.clone()) {
        (Shown::Notebook, Some(doc), _) => {
            view! { <LiveDocument socket=socket.clone() doc render=Render::Notebook(place.clone())/> }.into_any()
        }
        (Shown::Text { language }, Some(doc), _) => {
            view! { <LiveDocument socket=socket.clone() doc render=Render::Text(language.unwrap_or("txt"))/> }
                .into_any()
        }
        (_, _, Some(hash)) => stored_file(&entry.path, &hash, entry.size, &blobs),
        _ => view! { <p class="quiet">"This file has not been shared yet."</p> }.into_any(),
    }
    };
    view! {
        <div class="file-head">
            <h1 class="file-title">{history_entry.path.clone()}</h1>
            <button type="button" class="button" aria-pressed=move || in_history.get().to_string()
                on:click=move |_| in_history.update(|open| *open = !*open)>
                {move || if in_history.get() { "Close file history" } else { "File history" }}
            </button>
        </div>
        <p class="quiet">{about_line}</p>
        {move || if in_history.get() {
            view! {
                <FileHistory entry=history_entry.clone() blobs=history_blobs.clone()
                    place=history_place.clone() manifest=manifest.clone() in_history/>
            }.into_any()
        } else {
            live().into_any()
        }}
    }
}

/// A stored whole file: an image shows, anything else is a download.
fn stored_file(path: &str, hash: &str, size: Option<u64>, blobs: &Blobs) -> AnyView {
    let name = path.rsplit('/').next().unwrap_or_default().to_owned();
    if is_image(path) {
        return view! { <img class="file-image" alt=name src=blobs.url(hash)/> }.into_any();
    }
    let label = match size {
        Some(size) => format!("Download {name} ({})", size_text(size)),
        None => format!("Download {name}"),
    };
    view! {
        <p class="quiet">"The web app does not show this kind of file."</p>
        <a class="button" href=blobs.url(hash) download=name>{label}</a>
    }
    .into_any()
}

/// How many of the file list's versions a whole file's history lists.
// simplify: each version costs one read of the whole file list, so the history stops at the
// newest 40; ceiling = a file whose older changes are past that; upgrade = a server route
// that lists the versions where one path changed.
const WHOLE_FILE_VERSIONS: usize = 40;

/// A file's versions, the file as it was at the one picked, and Restore for editors.
#[component]
fn FileHistory(
    entry: Entry,
    blobs: Blobs,
    place: Place,
    manifest: String,
    in_history: RwSignal<bool>,
) -> impl IntoView {
    let room = expect_context::<Room>();
    let config = room.config.as_ref().clone();
    let editor = room.can_write();
    let versions = RwSignal::new(None::<Result<Vec<Version>, String>>);
    let picked = RwSignal::new(None::<u64>);
    let error: Error = RwSignal::new(None);
    let whole_file = entry.doc.is_none();
    let document = entry.doc.clone().unwrap_or_else(|| manifest.clone());
    {
        let (config, document, path) = (config.clone(), document.clone(), entry.path.clone());
        spawn_local(async move {
            let listed =
                file_versions(&config, &document, whole_file.then_some(path.as_str())).await;
            versions.try_set(Some(listed.map_err(|failure| failure.to_string())));
        });
    }
    let list = move || {
        match versions.get() {
        None => view! { <p class="quiet">"Reading the history…"</p> }.into_any(),
        Some(Err(why)) => view! { <p class="field-error" role="alert">{why}</p> }.into_any(),
        Some(Ok(listed)) if listed.is_empty() => view! { <p class="quiet">"No saved version of this file yet."</p> }.into_any(),
        Some(Ok(listed)) => view! {
            <ul class="file-versions">
                {listed.into_iter().map(|version| {
                    let id = version.id;
                    view! {
                        <li><button type="button" class="version" aria-current=move || (picked.get() == Some(id)).to_string()
                            on:click=move |_| { error.set(None); picked.set(Some(id)); }>
                            <span>{version_title(&version)}</span>
                            <small class="quiet">{version_when(&version)}</small>
                        </button></li>
                    }
                }).collect_view()}
            </ul>
        }.into_any(),
    }
    };
    let preview = {
        let (config, document, entry, blobs) = (
            config.clone(),
            document.clone(),
            entry.clone(),
            blobs.clone(),
        );
        move || {
            picked.get().map(|version| {
                view! {
                    <PastFile config=config.clone() document=document.clone() entry=entry.clone()
                        blobs=blobs.clone() place=place.clone() version/>
                }
            })
        }
    };
    let confirming = RwSignal::new(false);
    let restore = move |_| {
        let Some(version) = picked.get_untracked() else {
            return;
        };
        let (config, document) = (config.clone(), document.clone());
        let path = whole_file.then(|| entry.path.clone());
        spawn_local(async move {
            match api::restore_document(&config, version, &document, path.as_deref()).await {
                Ok(_) => {
                    in_history.try_set(false);
                }
                Err(failure) => {
                    error.try_set(Some(failure.to_string()));
                }
            }
        });
    };
    let restore = StoredValue::new(restore);
    view! {
        <div class="file-history">
            <aside class="file-version-list" aria-label="Versions">{list}</aside>
            <section class="file-past">
                {move || match picked.get() {
                    None => Some(view! { <p class="quiet">"Pick a version to see the file as it was."</p> }.into_any()),
                    Some(_) if editor => Some(view! {
                        <div class="restore-line">
                            <Show when=move || confirming.get()
                                fallback=move || view! {
                                    <button type="button" class="button" on:click=move |_| confirming.set(true)>"Restore this version"</button>
                                }>
                                <span>"The file goes back to this version for everyone."</span>
                                <button type="button" class="button primary" on:click=move |event| restore.with_value(|restore| restore(event))>"Restore"</button>
                                <button type="button" class="button" on:click=move |_| confirming.set(false)>"Cancel"</button>
                            </Show>
                        </div>
                    }.into_any()),
                    Some(_) => None,
                }}
                {error_line(error)}
                {preview}
            </section>
        </div>
    }
}

/// A notebook or text file's own versions; a whole file's are the file list's versions
/// where its entry changed.
async fn file_versions(
    config: &RoomConfig,
    document: &str,
    whole_file: Option<&str>,
) -> Result<Vec<Version>, api::ApiError> {
    let versions = api::document_history(config, document).await?;
    let Some(path) = whole_file else {
        return Ok(versions);
    };
    // One more than listed, so the oldest listed version is judged against the one before.
    // Read one after the other: the crate has no `futures` to join them.
    let mut contents = Vec::new();
    for version in versions.iter().take(WHOLE_FILE_VERSIONS + 1) {
        let state = api::document_state(config, version.id, document).await?;
        let hash = state["files"][path]["hash"].as_str().map(str::to_owned);
        contents.push((version.id, hash));
    }
    let kept = versions_that_changed(&contents);
    Ok(versions
        .into_iter()
        .take(WHOLE_FILE_VERSIONS)
        .filter(|version| kept.contains(&version.id))
        .collect())
}

/// The file as it was at `version`, shown like the live file.
#[component]
fn PastFile(
    config: RoomConfig,
    document: String,
    entry: Entry,
    blobs: Blobs,
    place: Place,
    version: u64,
) -> impl IntoView {
    let state = RwSignal::new(None::<Result<serde_json::Value, String>>);
    spawn_local(async move {
        let read = api::document_state(&config, version, &document).await;
        state.try_set(Some(read.map_err(|failure| failure.to_string())));
    });
    move || match state.get() {
        None => view! { <p class="quiet">"Reading this version…"</p> }.into_any(),
        Some(Err(why)) => view! { <p class="field-error" role="alert">{why}</p> }.into_any(),
        Some(Ok(json)) => match (shows_as(&entry), entry.doc.is_some()) {
            (Shown::Notebook, true) => {
                view! { <NotebookView notebook=notebook(&json) place=place.clone()/> }.into_any()
            }
            (Shown::Text { language }, true) => {
                let text = json["source"].as_str().unwrap_or_default().to_owned();
                view! { <pre class="code text-file"><code>{coloured(&text, language.unwrap_or("txt"))}</code></pre> }.into_any()
            }
            _ => {
                let stored = &json["files"][entry.path.as_str()];
                match stored["hash"].as_str() {
                    Some(hash) => stored_file(&entry.path, hash, stored["size"].as_u64(), &blobs),
                    None => view! { <p class="quiet">"The file was not in the SciQLop workspace at this version."</p> }.into_any(),
                }
            }
        },
    }
}

fn version_title(version: &Version) -> String {
    let who = version
        .author_email
        .as_deref()
        .map(person_text)
        .unwrap_or("Someone");
    match &version.name {
        Some(name) => format!("{name} · {who}"),
        None => who.to_owned(),
    }
}

fn version_when(version: &Version) -> String {
    version
        .ended_ms
        .or(version.started_ms)
        .map(|ms| minute_text(ms.saturating_mul(1_000_000)))
        .unwrap_or_default()
}

/// What a markdown cell's relative image path points at in this workspace, as a stored file.
fn placing(from: &str, files: Memo<Vec<Entry>>, blobs: Blobs) -> Place {
    let from = from.to_owned();
    Arc::new(move |dest: &str| {
        files
            .try_with_untracked(|files| {
                placed_image(&from, dest, files).and_then(|hash| blobs.url(hash))
            })
            .flatten()
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
    let Some(ws_url) = api_url(&socket, &[&doc]) else {
        return view! { <p class="field-error" role="alert">"The file cannot sync."</p> }
            .into_any();
    };
    let room = Room::new(RoomConfig { ws_url, ..config });
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
    let synced = Memo::new(move |was: Option<&bool>| {
        was.copied().unwrap_or(false) || room.status.get() == Status::Live
    });
    // simplify: every change re-renders the whole file; ceiling = a large notebook while
    // someone types in it; upgrade = a memo per cell id.
    (move || {
        if !synced.get() {
            return view! { <p class="quiet">"Opening the file…"</p> }.into_any();
        }
        room.track();
        let Ok(json) = document_json(&room.doc) else {
            return view! { <p class="field-error" role="alert">"This file cannot be shown."</p> }
                .into_any();
        };
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
