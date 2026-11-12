//! Export and import: the forms, the download, and reading a file the visitor picks or drops.

use cocat_core::model::{self, ShapeAxis};
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, BlobPropertyBag, DragEvent, File, HtmlAnchorElement, Url};
use yrs::ReadTxn;

use crate::edit::EditError;
use crate::route::Route;
use crate::state::Room;
use crate::transfer::{self, Format, Incoming, Summary};
use crate::views::actions::Form;
use crate::views::controls::{Error, Hint, error_line, go_to, now};
use crate::views::project_dialogs::Dialog;
use crate::views::projects::Projects;

/// The import dialog: `None` while closed, else open with the file dropped, if any.
#[derive(Clone, Copy)]
pub struct Importer {
    dialog: RwSignal<Option<Option<File>>, LocalStorage>,
    /// The first catalogue the last import made, for a page that wants to use it.
    pub imported: RwSignal<Option<String>>,
}

impl Importer {
    pub fn new() -> Self {
        Self {
            dialog: RwSignal::new_local(None),
            imported: RwSignal::new(None),
        }
    }

    pub fn open(self, file: Option<File>) {
        self.dialog.set(Some(file));
    }

    fn close(self) {
        self.dialog.set(None);
    }

    pub fn is_open(self) -> bool {
        self.dialog.with(Option::is_some)
    }
}

/// The catalogues in a format, saved by the browser as files: one, or one per catalogue
/// for TFCat.
fn save<T: ReadTxn>(
    txn: &T,
    uuids: &[String],
    format: Format,
    range: &Range,
) -> Result<(), String> {
    let value_range =
        transfer::value_range(&range.low.get_untracked(), &range.high.get_untracked())?;
    let made =
        transfer::files(txn, uuids, format, value_range).map_err(|error| error.to_string())?;
    for (name, text) in &made {
        download(name, format.mime(), text)
            .map_err(|_| "The browser refused to save the file.".to_owned())?;
    }
    Ok(())
}

/// A file made here, downloaded through a temporary link to a Blob.
fn download(name: &str, mime: &str, text: &str) -> Result<(), JsValue> {
    let options = BlobPropertyBag::new();
    options.set_type(mime);
    let parts = js_sys::Array::of1(&JsValue::from_str(text));
    let blob = Blob::new_with_str_sequence_and_options(&parts, &options)?;
    let url = Url::create_object_url_with_blob(&blob)?;
    let link: HtmlAnchorElement = document().create_element("a")?.unchecked_into();
    link.set_href(&url);
    link.set_download(name);
    let body = document().body().ok_or(JsValue::NULL)?;
    body.append_child(&link)?;
    link.click();
    link.remove();
    // Firefox reads the URL after the click returns: revoke it once the download started.
    let revoke = wasm_bindgen::closure::Closure::once_into_js(move || {
        let _ = Url::revoke_object_url(&url);
    });
    window()
        .set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), 10_000)?;
    Ok(())
}

#[component]
fn Formats(formats: &'static [Format], format: RwSignal<Format>) -> impl IntoView {
    view! {
        <fieldset class="formats">
            <legend class="field-label">"Format"</legend>
            {formats.iter().map(|&choice| view! {
                <label>
                    <input type="radio" name="format" prop:checked=move || format.get() == choice
                        on:change=move |_| format.set(choice)/>
                    {choice.label()}
                </label>
            }).collect_view()}
        </fieldset>
    }
}

/// The two boxes of the range a TFCat export draws value-less events over.
#[derive(Clone, Copy)]
struct Range {
    low: RwSignal<String>,
    high: RwSignal<String>,
}

impl Range {
    fn new() -> Self {
        Self {
            low: RwSignal::new(String::new()),
            high: RwSignal::new(String::new()),
        }
    }
}

/// Asks for a range when TFCat is picked and some events have no place on the shape axis.
#[component]
fn RangeBoxes(axis: Memo<Option<ShapeAxis>>, range: Range) -> impl IntoView {
    let boxes = move |axis: ShapeAxis| {
        let quantity = axis.quantity.clone();
        let unit = axis.unit.clone();
        let input = move |value: RwSignal<String>, label: String| {
            view! {
                <label>
                    <span class="field-label">{label}</span>
                    <input type="number" step="any" inputmode="decimal" prop:value=move || value.get()
                        on:input=move |event| value.set(event_target_value(&event))/>
                </label>
            }
        };
        view! {
            <fieldset class="value-range">
                <legend class="field-label">{format!("Range of {quantity}")}</legend>
                <p class="quiet">{format!(
                    "Some events have no {quantity}: TFCat draws each of them over this range."
                )}</p>
                <div class="value-range-boxes">
                    {input(range.low, format!("From ({unit})"))}
                    {input(range.high, format!("To ({unit})"))}
                </div>
            </fieldset>
        }
    };
    move || axis.get().map(boxes)
}

/// The shape axis to ask a range on, while TFCat is the format picked.
fn range_axis(
    room: StoredValue<Room, LocalStorage>,
    format: RwSignal<Format>,
    uuids: impl Fn() -> Vec<String> + Send + Sync + 'static,
) -> Memo<Option<ShapeAxis>> {
    Memo::new(move |_| {
        if format.get() != Format::Tfcat {
            return None;
        }
        let uuids = uuids();
        room.with_value(|room| room.read_untracked(|txn| transfer::needs_range(txn, &uuids)))
    })
}

/// The warnings a TFCat export gives, while TFCat is the format picked.
fn tfcat_notes(
    room: StoredValue<Room, LocalStorage>,
    format: RwSignal<Format>,
    uuids: impl Fn() -> Vec<String> + Send + Sync + 'static,
) -> Memo<Vec<String>> {
    Memo::new(move |_| {
        if format.get() != Format::Tfcat {
            return Vec::new();
        }
        let uuids = uuids();
        room.with_value(|room| room.read_untracked(|txn| transfer::tfcat_warnings(txn, &uuids)))
    })
}

#[component]
fn Notes(notes: Memo<Vec<String>>) -> impl IntoView {
    move || {
        notes
            .get()
            .into_iter()
            .map(|note| view! { <p class="quiet">{note}</p> })
            .collect_view()
    }
}

/// Under the heading: the catalogue on screen as a file.
#[component]
pub fn ExportForm(uuid: String, form: RwSignal<Form>) -> impl IntoView {
    let room = StoredValue::new_local(expect_context::<Room>());
    let format = RwSignal::new(Format::Votable);
    let error: Error = RwSignal::new(None);
    let range = Range::new();
    let uuids = vec![uuid];
    let axis = {
        let uuids = uuids.clone();
        range_axis(room, format, move || uuids.clone())
    };
    let notes = {
        let uuids = uuids.clone();
        tfcat_notes(room, format, move || uuids.clone())
    };
    let send = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let saved = room
            .with_value(|room| room.read_untracked(|txn| save(txn, &uuids, format.get(), &range)));
        match saved {
            Ok(()) => form.set(Form::Closed),
            Err(failure) => error.set(Some(failure)),
        }
    };
    view! {
        <form class="inline-form name-form export-form" on:submit=send>
            <Formats formats=&Format::ALL format/>
            <RangeBoxes axis range/>
            <Notes notes/>
            <Hint anchor="import-export" topic="import and export"
                text="A smart catalogue is saved with the events it holds now."/>
            {error_line(error)}
            <div class="row-buttons">
                <button type="submit" class="button primary">"Download"</button>
                <button type="button" class="button" on:click=move |_| form.set(Form::Closed)>"Cancel"</button>
            </div>
        </form>
    }
}

/// The project menu's export: several catalogues in one file.
#[component]
pub fn ExportDialog(dialog: RwSignal<Option<Dialog>>) -> impl IntoView {
    let room = StoredValue::new_local(expect_context::<Room>());
    let listed = room.with_value(|room| room.read_untracked(|txn| model::catalogues(txn)));
    let picked = RwSignal::new(
        listed
            .iter()
            .map(|catalogue| catalogue.uuid.clone())
            .collect::<Vec<_>>(),
    );
    let format = RwSignal::new(Format::Votable);
    let error: Error = RwSignal::new(None);
    let range = Range::new();
    let axis = range_axis(room, format, move || picked.get());
    let notes = tfcat_notes(room, format, move || picked.get());
    let send = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let uuids = picked.get_untracked();
        let saved = room
            .with_value(|room| room.read_untracked(|txn| save(txn, &uuids, format.get(), &range)));
        match saved {
            Ok(()) => dialog.set(None),
            Err(failure) => error.set(Some(failure)),
        }
    };
    let toggle = move |uuid: String| {
        picked.update(
            |picked| match picked.iter().position(|kept| *kept == uuid) {
                Some(index) => {
                    picked.remove(index);
                }
                None => picked.push(uuid),
            },
        );
    };
    let entry = move |catalogue: model::CatalogueSummary| {
        let uuid = catalogue.uuid.clone();
        let checked = {
            let uuid = uuid.clone();
            move || picked.with(|picked| picked.contains(&uuid))
        };
        view! {
            <label>
                <input type="checkbox" prop:checked=checked on:change=move |_| toggle(uuid.clone())/>
                <span class="label">{catalogue.name}</span>
            </label>
        }
    };
    let empty = listed.is_empty();
    view! {
        <form class="name-form" on:submit=send>
            <h2>"Export catalogues"</h2>
            <fieldset class="export-list">
                <legend class="field-label">"Catalogues"</legend>
                {listed.into_iter().map(entry).collect_view()}
                {empty.then(|| view! { <p class="quiet">"No catalogues in this project yet."</p> })}
            </fieldset>
            <Formats formats=&Format::MANY format/>
            <RangeBoxes axis range/>
            <Notes notes/>
            <Hint anchor="import-export" topic="import and export"
                text="VOTable and tscat JSON put them all in one file; TFCat makes one file per catalogue. For CSV or HPEvent, export one catalogue from its Catalogue menu."/>
            {error_line(error)}
            <div class="row-buttons">
                <button type="submit" class="button primary"
                    disabled=move || picked.with(Vec::is_empty)>"Download"</button>
                <button type="button" class="button" on:click=move |_| dialog.set(None)>"Cancel"</button>
            </div>
        </form>
    }
}

/// Where the import dialog is.
#[derive(Clone, PartialEq)]
enum Stage {
    Pick,
    Reading(String),
    Ready(Summary, Vec<String>),
    Writing,
}

/// Lets the browser paint before a long piece of work: one frame, then a task.
async fn breathe() {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 0);
    });
    let frame = js_sys::Promise::new(&mut |resolve, _| {
        let _ = window().request_animation_frame(&resolve);
    });
    let _ = JsFuture::from(frame).await;
    let _ = JsFuture::from(promise).await;
}

async fn text_of(file: &File) -> Result<String, String> {
    JsFuture::from(file.text())
        .await
        .ok()
        .and_then(|text| text.as_string())
        .ok_or_else(|| "The browser could not read the file.".to_owned())
}

/// Reads a file, shows what importing it does, and writes it as one change.
#[component]
pub fn ImportDialog() -> impl IntoView {
    let importer = expect_context::<Importer>();
    let projects = expect_context::<Projects>();
    let room = StoredValue::new_local(expect_context::<Room>());
    let stage = RwSignal::new(Stage::Pick);
    let incoming = StoredValue::new_local(None::<Incoming>);
    let error: Error = RwSignal::new(None);
    let node = NodeRef::<html::Dialog>::new();
    Effect::new(move |_| {
        if let Some(node) = node.get() {
            let _ = node.show_modal();
        }
    });
    let take = move |file: File| {
        error.set(None);
        stage.set(Stage::Reading(file.name()));
        spawn_local(async move {
            let read = match text_of(&file).await {
                Ok(text) => {
                    breathe().await;
                    transfer::read(&text, &file.name()).map_err(|failure| failure.to_string())
                }
                Err(failure) => Err(failure),
            };
            match read {
                Ok(read) => {
                    let summary = room.with_value(|room| {
                        room.read_untracked(|txn| transfer::summary(txn, &read))
                    });
                    let warnings = transfer::warnings(&read).to_vec();
                    incoming.set_value(Some(read));
                    stage.try_set(Stage::Ready(summary, warnings));
                }
                Err(failure) => {
                    error.try_set(Some(failure));
                    stage.try_set(Stage::Pick);
                }
            }
        });
    };
    if let Some(Some(file)) = importer.dialog.get_untracked() {
        take(file);
    }
    let picked = move |event: leptos::ev::Event| {
        let input: web_sys::HtmlInputElement = event_target(&event);
        if let Some(file) = input.files().and_then(|files| files.get(0)) {
            take(file);
        }
    };
    let dropped = move |event: DragEvent| {
        event.prevent_default();
        if let Some(file) = dropped_file(&event) {
            take(file);
        }
    };
    let import = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        stage.set(Stage::Writing);
        spawn_local(async move {
            breathe().await;
            let Some(now) = now() else { return };
            let written = incoming.with_value(|read| {
                let read = read.as_ref()?;
                Some(room.with_value(|room| {
                    room.write(|txn| transfer::write(txn, read, now).map_err(EditError::from))
                }))
            });
            let Some(written) = written else { return };
            match written {
                Ok(written) => {
                    importer.close();
                    let first = written.catalogues.first().cloned();
                    importer.imported.set(first.clone());
                    let starting_a_round = projects.route.get_untracked() == Route::NewRound;
                    if let Some(first) = first.filter(|_| !starting_a_round) {
                        go_to(&Route::Catalogue(first).hash());
                    }
                }
                Err(failure) => {
                    error.try_set(Some(failure.to_string()));
                    stage.try_set(Stage::Pick);
                }
            }
        });
    };
    let body = move || {
        match stage.get() {
        Stage::Pick => view! {
            <label class="drop-zone" on:dragover=|event: DragEvent| event.prevent_default() on:drop=dropped>
                <span>"Drop a file here, or"</span>
                <input type="file" accept=Format::accept()
                    on:change=picked/>
            </label>
        }
        .into_any(),
        Stage::Reading(name) => view! {
            <p class="quiet" role="status">{format!("Reading {name}…")}</p>
        }
        .into_any(),
        Stage::Ready(summary, warnings) => view! {
            <p class="import-summary" role="status">{summary.text()}</p>
            {warnings.into_iter().map(|warning| view! { <p class="quiet">{warning}</p> }).collect_view()}
        }
        .into_any(),
        Stage::Writing => view! {
            <p class="quiet" role="status">"Importing…"</p>
        }
        .into_any(),
    }
    };
    let ready = move || matches!(stage.get(), Stage::Ready(..));
    view! {
        <dialog class="project-dialog" node_ref=node on:close=move |_| importer.close()>
            <form class="name-form import-form" on:submit=import>
                <h2>"Import a file"</h2>
                <Hint anchor="import-export" topic="import and export"
                    text="A VOTable (.xml), a tscat JSON or TFCat file (.json), a CSV file, or an HPEvent list (.txt, as CDAWeb and SPASE publish them). Events whose id the project already holds are updated."/>
                {body}
                {error_line(error)}
                <div class="row-buttons">
                    <button type="submit" class="button primary" disabled=move || !ready()>"Import"</button>
                    <button type="button" class="button" on:click=move |_| importer.close()>"Cancel"</button>
                </div>
            </form>
        </dialog>
    }
}

fn dropped_file(event: &DragEvent) -> Option<File> {
    event.data_transfer()?.files()?.get(0)
}

/// Dropping a file on the catalogue area opens the import with it, for editors.
pub fn drop_on(
    importer: Importer,
    editor: bool,
) -> (impl Fn(DragEvent) + Copy, impl Fn(DragEvent) + Copy) {
    let carries_files = |event: &DragEvent| {
        event
            .data_transfer()
            .is_some_and(|transfer| transfer.types().includes(&JsValue::from_str("Files"), 0))
    };
    let over = move |event: DragEvent| {
        if editor && carries_files(&event) {
            event.prevent_default();
        }
    };
    let drop = move |event: DragEvent| {
        if !editor {
            return;
        }
        event.prevent_default();
        if let Some(file) = dropped_file(&event) {
            importer.open(Some(file));
        }
    };
    (over, drop)
}
