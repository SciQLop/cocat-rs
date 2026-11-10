//! Uploading files into a project's SciQLop workspace: the pickers, the run, and its report.

use cocat_core::files::{
    DEFAULT_PATTERNS, FileKind, ManifestEntry, Plan, Rules, Skip, patterns, plan, put_entry,
    set_patterns, write_notebook, write_text,
};
use cocat_core::time::Timestamp;
use js_sys::Uint8Array;
use leptos::html::Input;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;
use serde_json::{Value, json};
use wasm_bindgen_futures::JsFuture;
use web_sys::{File, FileList};
use yrs::{Transact, TransactionMut};

use crate::api;
use crate::connection;
use crate::projects::api_url;
use crate::session::RoomConfig;
use crate::state::Room;
use crate::sync::Status;
use crate::upload::{Failure, failure_text, kept_document, path_of, sha256_hex, uploaded_text};
use crate::views::files::{FilesState, ProjectFiles, manifest_files};
use crate::workspace::Entry;

/// Used when the server does not say: the server's own default.
const DEFAULT_FILE_MAX: u64 = 20 * 1024 * 1024;

/// The page's uploads: the hidden pickers, what is running, and the last report.
#[derive(Clone, Copy)]
pub struct Uploads {
    pick_files: NodeRef<Input>,
    pick_folder: NodeRef<Input>,
    progress: RwSignal<Option<String>>,
    report: RwSignal<Option<(String, Vec<String>)>>,
    /// Whether the project has its file list, so the page syncs it.
    pub files_on: RwSignal<bool>,
}

impl Uploads {
    pub fn provide(files_on: bool) -> Self {
        let uploads = Self {
            pick_files: NodeRef::new(),
            pick_folder: NodeRef::new(),
            progress: RwSignal::new(None),
            report: RwSignal::new(None),
            files_on: RwSignal::new(files_on),
        };
        provide_context(uploads);
        uploads
    }

    /// Opens the browser's picker, for files or for one folder.
    pub fn pick(self, folder: bool) {
        let input = if folder {
            self.pick_folder
        } else {
            self.pick_files
        };
        if let Some(input) = input.get_untracked() {
            input.click();
        }
    }

    /// Uploads `picked` into the project `config` names, whose files `state` holds.
    pub fn start(self, config: RoomConfig, state: ProjectFiles, picked: Vec<File>) {
        if picked.is_empty() || self.progress.get_untracked().is_some() {
            return;
        }
        self.report.set(None);
        self.progress.set(Some("Uploading…".to_owned()));
        spawn_local(async move {
            let report = run(&config, state, self, picked).await;
            self.progress.set(None);
            self.report.set(Some(report));
        });
    }
}

/// The largest notebook or text file sent as one live update: the server takes frames of
/// up to 16 MB.
const LIVE_MAX: u64 = 15 * 1024 * 1024;

/// What an upload checks each file against: the files listed now and the sharing rules.
struct Target {
    files: Vec<Entry>,
    rules: Rules,
    cap: u64,
}

async fn run(
    config: &RoomConfig,
    state: ProjectFiles,
    uploads: Uploads,
    picked: Vec<File>,
) -> (String, Vec<String>) {
    let shown = state.with_untracked(|state| match state {
        FilesState::Ready(files) => Some(files.manifest().to_owned()),
        _ => None,
    });
    let manifest = match shown.clone() {
        Some(id) => Ok(id),
        None => make_manifest(config).await,
    };
    let manifest = match manifest {
        Ok(id) => open(config, &id).await,
        Err(why) => Err(why),
    };
    let manifest = match manifest {
        Ok(manifest) => manifest,
        Err(why) => return (format!("Nothing was uploaded: {why}"), Vec::new()),
    };
    let target = manifest.room.read_untracked(|txn| Target {
        files: manifest_files(txn),
        rules: Rules::new(&patterns(txn).unwrap_or_else(|| DEFAULT_PATTERNS.to_owned())),
        cap: config.file_max.unwrap_or(DEFAULT_FILE_MAX),
    });
    let (entries, failures) = upload_all(config, &target, uploads, &picked).await;
    let count = entries.len();
    let listed = manifest
        .write(move |txn| {
            if patterns(txn).is_none() {
                set_patterns(txn, DEFAULT_PATTERNS);
            }
            for (path, entry) in &entries {
                put_entry(txn, path, entry);
            }
            Ok(())
        })
        .await;
    if shown.is_none() {
        uploads.files_on.set(true);
        state.set(FilesState::Loading);
    }
    match listed {
        Ok(()) => (uploaded_text(count), failures),
        Err(why) => (
            format!("The file list could not be written: {why}"),
            failures,
        ),
    }
}

async fn upload_all(
    config: &RoomConfig,
    target: &Target,
    uploads: Uploads,
    picked: &[File],
) -> (Vec<(String, ManifestEntry)>, Vec<String>) {
    let (mut entries, mut failures) = (Vec::new(), Vec::new());
    for (index, file) in picked.iter().enumerate() {
        let progress = format!("Uploading {} of {}…", index + 1, picked.len());
        uploads.progress.set(Some(progress));
        let path = path_of(&file.name(), &relative_path(file));
        match upload_one(config, target, file, &path).await {
            Ok(entry) => entries.push((path, entry)),
            Err(failure) => failures.push(failure_text(&path, &failure)),
        }
    }
    (entries, failures)
}

#[derive(Deserialize)]
struct Made {
    id: String,
}

#[derive(Deserialize)]
struct Listed {
    id: String,
    kind: String,
}

/// A project gets its file list once: when another page made it first, that one is used.
async fn make_manifest(config: &RoomConfig) -> Result<String, String> {
    let documents = ["rooms", config.room_id.as_str(), "documents"];
    let made = api::create::<Made>(&config.api_url, &documents, json!({"kind": "manifest"})).await;
    if let Ok(made) = made {
        return Ok(made.id);
    }
    let listed = api::read::<Vec<Listed>>(&config.api_url, &documents)
        .await
        .map_err(|error| error.to_string())?;
    listed
        .into_iter()
        .find(|document| document.kind == "manifest")
        .map(|document| document.id)
        .ok_or_else(|| "the project's file list could not be made".to_owned())
}

/// What a file becomes, or why it is skipped.
fn kind_of(target: &Target, path: &str, size: u64) -> Result<FileKind, Failure> {
    let kind = match plan(path, size, &target.rules, target.cap) {
        Plan::Skip(skip) => return Err(Failure::Skipped(skip)),
        Plan::Notebook => FileKind::Notebook,
        Plan::Text => FileKind::Text,
        Plan::File => FileKind::File,
    };
    if kind != FileKind::File && size > LIVE_MAX {
        return Err(Failure::Skipped(Skip::TooBig { cap: LIVE_MAX }));
    }
    Ok(kind)
}

async fn upload_one(
    config: &RoomConfig,
    target: &Target,
    file: &File,
    path: &str,
) -> Result<ManifestEntry, Failure> {
    let size = file.size() as u64;
    let kind = kind_of(target, path, size)?;
    let bytes = bytes_of(file).await.map_err(Failure::Unreadable)?;
    let (kind, doc, hash) = match (kind, String::from_utf8(bytes)) {
        (FileKind::Notebook, Ok(text)) => (
            kind,
            Some(upload_notebook(config, target, path, &text).await?),
            None,
        ),
        (FileKind::Text, Ok(text)) => (
            kind,
            Some(upload_text(config, target, path, text).await?),
            None,
        ),
        (FileKind::Notebook, Err(_)) => {
            return Err(Failure::Unreadable(
                "not a notebook (not UTF-8 text)".to_owned(),
            ));
        }
        // Text that is not UTF-8 is kept whole, like any other file.
        (_, bytes) => {
            let bytes = bytes.map_or_else(|error| error.into_bytes(), String::into_bytes);
            (
                FileKind::File,
                None,
                Some(store_blob(config, &bytes).await?),
            )
        }
    };
    Ok(ManifestEntry {
        kind,
        doc,
        hash,
        size,
        by: Some(config.user_email.clone()),
        at: Timestamp::now().unix_millis(),
    })
}

/// Checks the notebook on a scratch document first, so a bad one never makes a document.
async fn upload_notebook(
    config: &RoomConfig,
    target: &Target,
    path: &str,
    text: &str,
) -> Result<String, Failure> {
    let notebook: Value = serde_json::from_str(text)
        .map_err(|_| Failure::Unreadable("not a notebook (not JSON)".to_owned()))?;
    write_notebook(&mut yrs::Doc::new().transact_mut(), &notebook)
        .map_err(|error| Failure::Unreadable(format!("not a notebook: {error}")))?;
    live_document(config, target, path, FileKind::Notebook, move |txn| {
        write_notebook(txn, &notebook).map_err(|error| error.to_string())
    })
    .await
}

async fn upload_text(
    config: &RoomConfig,
    target: &Target,
    path: &str,
    text: String,
) -> Result<String, Failure> {
    live_document(config, target, path, FileKind::Text, move |txn| {
        write_text(txn, &text);
        Ok(())
    })
    .await
}

/// Writes a notebook or a text file into the document `path` already has, or a new one.
async fn live_document(
    config: &RoomConfig,
    target: &Target,
    path: &str,
    kind: FileKind,
    write: impl FnOnce(&mut TransactionMut<'_>) -> Result<(), String>,
) -> Result<String, Failure> {
    let doc = match kept_document(&target.files, path, kind) {
        Some(doc) => doc,
        None => new_document(config, kind).await.map_err(Failure::Refused)?,
    };
    let document = open(config, &doc).await.map_err(Failure::Refused)?;
    document.write(write).await.map_err(Failure::Refused)?;
    Ok(doc)
}

async fn new_document(config: &RoomConfig, kind: FileKind) -> Result<String, String> {
    let kind = match kind {
        FileKind::Notebook => "notebook",
        _ => "text",
    };
    api::create::<Made>(
        &config.api_url,
        &["rooms", &config.room_id, "documents"],
        json!({ "kind": kind }),
    )
    .await
    .map(|made| made.id)
    .map_err(|error| error.to_string())
}

async fn store_blob(config: &RoomConfig, bytes: &[u8]) -> Result<String, Failure> {
    let hash = sha256_hex(bytes);
    api::put_bytes(
        &config.api_url,
        &["rooms", &config.room_id, "blobs", &hash],
        bytes,
    )
    .await
    .map_err(|error| Failure::Refused(error.to_string()))?;
    Ok(hash)
}

async fn bytes_of(file: &File) -> Result<Vec<u8>, String> {
    let buffer = JsFuture::from(file.array_buffer()).await.map_err(|_| {
        "the browser could not read it (to upload a folder, use Upload a folder…)".to_owned()
    })?;
    Ok(Uint8Array::new(&buffer).to_vec())
}

/// How long a document's socket may take to reach the server's state.
const SYNC_WAIT_MS: u32 = 15_000;
const POLL_MS: u32 = 100;

/// One document of the project, open on a socket of its own while the upload writes it.
struct Opened {
    room: Room,
    connection: connection::Connection,
    owner: Owner,
}

/// Opens document `doc` and waits until it holds the server's state.
async fn open(config: &RoomConfig, doc: &str) -> Result<Opened, String> {
    let ws_url = api_url(&config.ws_url, &[doc]).ok_or("the address is invalid")?;
    let owner = Owner::new();
    let room = owner.with(|| {
        Room::new(RoomConfig {
            ws_url,
            ..config.clone()
        })
    });
    let connection = owner
        .with(|| connection::connect(room.clone()))
        .map_err(|error| error.to_string())?;
    let opened = Opened {
        room,
        connection,
        owner,
    };
    if wait_live(&opened.room).await {
        return Ok(opened);
    }
    opened.close();
    Err("the server did not answer in time".to_owned())
}

impl Opened {
    /// Applies `edit` and closes the document once the server confirmed it has the edit: it
    /// answers a fresh sync only after the frames sent before, and closes the socket on a
    /// refused one (too large, over the storage quota).
    async fn write(
        self,
        edit: impl FnOnce(&mut TransactionMut<'_>) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut outcome = Ok(());
        let written = self.room.write(|txn| {
            outcome = edit(txn);
            Ok(())
        });
        let drops = self.connection.drops();
        self.connection.resync();
        let confirmed = wait_live(&self.room).await && self.connection.drops() == drops;
        self.close();
        written.map_err(|error| error.to_string())?;
        outcome?;
        match confirmed {
            true => Ok(()),
            false => Err("the server refused it (too large, or the project is full)".to_owned()),
        }
    }

    fn close(self) {
        self.connection.close();
        self.owner.cleanup();
    }
}

async fn wait_live(room: &Room) -> bool {
    for _ in 0..SYNC_WAIT_MS / POLL_MS {
        if room.status.get_untracked() == Status::Live {
            return true;
        }
        sleep(POLL_MS).await;
    }
    false
}

async fn sleep(ms: u32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32);
    });
    let _ = JsFuture::from(promise).await;
}

/// A folder upload's file says where it sits in the folder (not in web-sys' bindings).
fn relative_path(file: &File) -> String {
    js_sys::Reflect::get(file, &"webkitRelativePath".into())
        .ok()
        .and_then(|path| path.as_string())
        .unwrap_or_default()
}

fn files_of(list: Option<FileList>) -> Vec<File> {
    let Some(list) = list else {
        return Vec::new();
    };
    (0..list.length())
        .filter_map(|index| list.get(index))
        .collect()
}

/// The hidden pickers the menus open, and the upload's progress and report.
#[component]
pub fn UploadPanel(config: RoomConfig, state: ProjectFiles) -> impl IntoView {
    let uploads = expect_context::<Uploads>();
    let start = move |input: NodeRef<Input>| {
        let Some(element) = input.get_untracked() else {
            return;
        };
        let picked = files_of(element.files());
        element.set_value("");
        uploads.start(config.clone(), state, picked);
    };
    let start = StoredValue::new_local(start);
    // The view macro knows no `webkitdirectory`, the attribute that picks a whole folder.
    Effect::new(move |_| {
        if let Some(input) = uploads.pick_folder.get() {
            let _ = input.set_attribute("webkitdirectory", "");
        }
    });
    view! {
        <input type="file" multiple hidden node_ref=uploads.pick_files
            on:change=move |_| start.with_value(|start| start(uploads.pick_files))/>
        <input type="file" hidden node_ref=uploads.pick_folder
            on:change=move |_| start.with_value(|start| start(uploads.pick_folder))/>
        {move || uploads.progress.get().map(|text| view! {
            <p class="upload-status quiet" role="status">{text}</p>
        })}
        {move || uploads.report.get().map(|(text, failures)| view! {
            <div class="upload-report" role="status">
                <p>{text}</p>
                {(!failures.is_empty()).then(|| view! {
                    <ul>{failures.into_iter().map(|line| view! { <li>{line}</li> }).collect_view()}</ul>
                })}
                <button type="button" class="button" on:click=move |_| uploads.report.set(None)>"Close"</button>
            </div>
        })}
    }
}

/// Dropping files on the Files section uploads them, for editors.
pub fn drop_files(
    config: RoomConfig,
    state: ProjectFiles,
) -> (
    impl Fn(web_sys::DragEvent) + Clone,
    impl Fn(web_sys::DragEvent) + Clone,
) {
    let uploads = expect_context::<Uploads>();
    let over = |event: web_sys::DragEvent| event.prevent_default();
    let drop = move |event: web_sys::DragEvent| {
        event.prevent_default();
        event.stop_propagation();
        let picked = files_of(event.data_transfer().and_then(|transfer| transfer.files()));
        uploads.start(config.clone(), state, picked);
    };
    (over, drop)
}
