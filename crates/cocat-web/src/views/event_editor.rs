//! The event panel's controls. Every input commits on change (blur or Enter), not on each
//! keystroke, and shows its own error; a refused value writes nothing.

use leptos::html::Input;
use leptos::prelude::*;
use yrs::TransactionMut;

use crate::catalogues::Named;
use crate::detail::EventDetail;
use crate::edit::{self, EditError, STATUSES};
use crate::rows::Row;
use crate::state::Room;
use crate::views::controls::{Error, commit, confirm, error_line};
use crate::views::event_panel::head;

type Detail = Memo<Option<EventDetail>>;

type Write = fn(&mut TransactionMut<'_>, &str, &str) -> Result<(), EditError>;
type SetAttribute = fn(&mut TransactionMut<'_>, &str, &str, &str) -> Result<(), EditError>;

/// How the tag and attribute controls write: to an event or to a catalogue.
#[derive(Clone, Copy)]
pub struct Writes {
    add_tags: Write,
    remove_tag: Write,
    set_attribute: SetAttribute,
    remove_attribute: Write,
}

const EVENT: Writes = Writes {
    add_tags: edit::add_tags,
    remove_tag: edit::remove_tag,
    set_attribute: edit::set_attribute,
    remove_attribute: edit::remove_attribute,
};

pub const CATALOGUE: Writes = Writes {
    add_tags: edit::add_catalogue_tags,
    remove_tag: edit::remove_catalogue_tag,
    set_attribute: edit::set_catalogue_attribute,
    remove_attribute: edit::remove_catalogue_attribute,
};

/// The event or catalogue the controls write to.
#[derive(Clone, Copy)]
pub struct Target {
    room: StoredValue<Room, LocalStorage>,
    uuid: StoredValue<String, LocalStorage>,
    writes: Writes,
}

impl Target {
    pub fn new(uuid: String, writes: Writes) -> Self {
        Self {
            room: StoredValue::new_local(expect_context::<Room>()),
            uuid: StoredValue::new_local(uuid),
            writes,
        }
    }

    fn edit(
        self,
        error: Error,
        change: impl FnOnce(&mut TransactionMut<'_>, &str) -> Result<(), EditError>,
    ) {
        self.room.with_value(|room| {
            self.uuid
                .with_value(|uuid| commit(room, error, |txn| change(txn, uuid)))
        });
    }
}

fn field<T: PartialEq + Send + Sync + 'static>(
    detail: Detail,
    read: impl Fn(&EventDetail) -> T + Send + Sync + 'static,
) -> Memo<Option<T>> {
    Memo::new(move |_| detail.with(|detail| detail.as_ref().map(&read)))
}

fn new_error() -> Error {
    RwSignal::new(None)
}

#[component]
pub fn Editor(uuid: String, detail: Detail) -> impl IntoView {
    let target = Target::new(uuid, EVENT);
    let tags = field(detail, |detail| detail.tags.clone());
    let attributes = field(detail, |detail| detail.attributes.clone());
    view! {
        {move || detail.with(|detail| detail.as_ref().map(head))}
        <Range target detail/>
        <div class="field-grid">
            <Status target detail/>
            <Confidence target detail/>
        </div>
        <Tags target tags/>
        <Attributes target attributes/>
        <Comment target detail/>
        <Listing target detail/>
        <Facts detail/>
        <TrashButton target/>
    }
}

#[component]
fn Range(target: Target, detail: Detail) -> impl IntoView {
    let error = new_error();
    let (start, stop) = (NodeRef::<Input>::new(), NodeRef::<Input>::new());
    let start_text = field(detail, |detail| detail.start.clone());
    let stop_text = field(detail, |detail| detail.stop_time.clone());
    let save = move |_| {
        let value =
            |input: NodeRef<Input>| input.get().map(|input| input.value()).unwrap_or_default();
        let (start, stop) = (value(start), value(stop));
        target.edit(error, |txn, uuid| edit::set_range(txn, uuid, &start, &stop));
    };
    view! {
        <div class="field-stack">
            <label class="field">
                <span class="field-label">"Start (UTC)"</span>
                <input type="text" class="mono" node_ref=start spellcheck="false"
                    prop:value=move || start_text.get().unwrap_or_default() on:change=save/>
            </label>
            <label class="field">
                <span class="field-label">"Stop (UTC)"</span>
                <input type="text" class="mono" node_ref=stop spellcheck="false"
                    prop:value=move || stop_text.get().unwrap_or_default() on:change=save/>
            </label>
        </div>
        {error_line(error)}
    }
}

#[component]
fn Status(target: Target, detail: Detail) -> impl IntoView {
    let error = new_error();
    let status = field(detail, |detail| detail.status.clone());
    let save = move |event| {
        let name = event_target_value(&event);
        target.edit(error, |txn, uuid| edit::set_status(txn, uuid, &name));
    };
    view! {
        <label class="field">
            <span class="field-label">"Status"</span>
            <select on:change=save>
                {STATUSES.map(|option| {
                    let name = option.name();
                    view! {
                        <option value=name prop:selected=move || status.get().as_deref() == Some(name)>
                            {name}
                        </option>
                    }
                }).collect_view()}
            </select>
            {error_line(error)}
        </label>
    }
}

#[component]
fn Confidence(target: Target, detail: Detail) -> impl IntoView {
    let error = new_error();
    let text = field(detail, |detail| {
        detail
            .confidence
            .map(|confidence| confidence.to_string())
            .unwrap_or_default()
    });
    let save = move |event| {
        let text = event_target_value(&event);
        target.edit(error, |txn, uuid| edit::set_confidence(txn, uuid, &text));
    };
    // A text input, not a number one: a number input hides what was typed when it is not
    // a number, and an empty value would then remove the confidence.
    view! {
        <label class="field">
            <span class="field-label">"Confidence"</span>
            <input type="text" inputmode="decimal" class="mono" placeholder="0 to 1"
                prop:value=move || text.get().unwrap_or_default() on:change=save/>
            {error_line(error)}
        </label>
    }
}

#[component]
pub fn Tags(target: Target, tags: Memo<Option<Vec<String>>>) -> impl IntoView {
    let error = new_error();
    let writes = target.writes;
    let add = move |event: leptos::ev::Event| {
        let list = event_target_value(&event);
        target.edit(error, |txn, uuid| (writes.add_tags)(txn, uuid, &list));
        if error.with_untracked(Option::is_none) {
            event_target::<web_sys::HtmlInputElement>(&event).set_value("");
        }
    };
    let chip = move |tag: String| {
        let label = format!("Remove tag {tag}");
        let removed = tag.clone();
        let remove = move |_| {
            target.edit(error, |txn, uuid| (writes.remove_tag)(txn, uuid, &removed));
        };
        view! {
            <span class="tag editable">
                {tag.clone()}
                <button type="button" aria-label=label on:click=remove>"×"</button>
            </span>
        }
    };
    view! {
        <section>
            <h3 class="section-label">"Tags"</h3>
            <div class="tags">
                <For each=move || tags.get().unwrap_or_default() key=Clone::clone children=chip/>
            </div>
            <input type="text" class="add" placeholder="Add tags, comma separated"
                aria-label="Add tags" on:change=add/>
            {error_line(error)}
        </section>
    }
}

#[component]
pub fn Attributes(
    target: Target,
    attributes: Memo<Option<Vec<(String, String)>>>,
) -> impl IntoView {
    let error = new_error();
    let writes = target.writes;
    let row = move |(key, value): (String, String)| {
        let change = {
            let key = key.clone();
            move |event| {
                let value = event_target_value(&event);
                target.edit(error, |txn, uuid| {
                    (writes.set_attribute)(txn, uuid, &key, &value)
                });
            }
        };
        let remove = {
            let key = key.clone();
            move |_| {
                target.edit(error, |txn, uuid| {
                    (writes.remove_attribute)(txn, uuid, &key)
                })
            }
        };
        view! {
            <dt>{key.clone()}</dt>
            <dd class="attribute">
                <input type="text" class="mono" aria-label=key.clone() value=value on:change=change/>
                <button type="button" class="icon" aria-label=format!("Remove {key}") on:click=remove>"×"</button>
            </dd>
        }
    };
    let (new_key, new_value) = (NodeRef::<Input>::new(), NodeRef::<Input>::new());
    let add = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let value =
            |input: NodeRef<Input>| input.get().map(|input| input.value()).unwrap_or_default();
        let (key, value_text) = (value(new_key), value(new_value));
        target.edit(error, |txn, uuid| {
            (writes.set_attribute)(txn, uuid, &key, &value_text)
        });
        if error.with_untracked(Option::is_none) {
            for input in [new_key, new_value] {
                if let Some(input) = input.get() {
                    input.set_value("");
                }
            }
        }
    };
    view! {
        <section>
            <h3 class="section-label">"Attributes"</h3>
            <dl class="attributes">
                <For each=move || attributes.get().unwrap_or_default() key=Clone::clone children=row/>
            </dl>
            <form class="add-attribute" on:submit=add>
                <input type="text" placeholder="name" aria-label="New attribute name" node_ref=new_key/>
                <input type="text" class="mono" placeholder="value" aria-label="New attribute value" node_ref=new_value/>
                <button type="submit" class="button">"Add"</button>
            </form>
            {error_line(error)}
        </section>
    }
}

#[component]
fn Comment(target: Target, detail: Detail) -> impl IntoView {
    let error = new_error();
    let comment = field(detail, |detail| detail.comment.clone().unwrap_or_default());
    let save = move |event| {
        let text = event_target_value(&event);
        target.edit(error, |txn, uuid| edit::set_comment(txn, uuid, &text));
    };
    view! {
        <section>
            <label class="field">
                <span class="section-label">"Comment"</span>
                <textarea rows="3" prop:value=move || comment.get().unwrap_or_default() on:change=save></textarea>
            </label>
            {error_line(error)}
        </section>
    }
}

#[component]
fn Listing(target: Target, detail: Detail) -> impl IntoView {
    let error = new_error();
    let listed = field(detail, |detail| detail.catalogues.clone());
    let elsewhere = field(detail, |detail| detail.elsewhere.clone());
    let entry = move |named: Named| {
        let remove = {
            let catalogue = named.uuid.clone();
            move |_| target.edit(error, |txn, uuid| edit::unlist_event(txn, &catalogue, uuid))
        };
        view! {
            <li class="listed">
                <span>{named.name.clone()}</span>
                <button type="button" class="icon" aria-label=format!("Take out of {}", named.name)
                    on:click=remove>"×"</button>
            </li>
        }
    };
    let add = move |event: leptos::ev::Event| {
        let catalogue = event_target_value(&event);
        if !catalogue.is_empty() {
            target.edit(error, |txn, uuid| edit::list_event(txn, &catalogue, uuid));
        }
        event_target::<web_sys::HtmlSelectElement>(&event).set_value("");
    };
    view! {
        <section>
            <h3 class="section-label">"Listed in"</h3>
            <ul class="plain">
                <For each=move || listed.get().unwrap_or_default() key=Clone::clone children=entry/>
            </ul>
            <select class="add" aria-label="Add to a catalogue" on:change=add>
                <option value="" selected>"Add to a catalogue…"</option>
                {move || elsewhere.get().unwrap_or_default().into_iter().map(|named| view! {
                    <option value=named.uuid>{named.name}</option>
                }).collect_view()}
            </select>
            {error_line(error)}
        </section>
    }
}

#[component]
fn Facts(detail: Detail) -> impl IntoView {
    let fact = move |label: &'static str, read: fn(&EventDetail) -> Option<String>| {
        let value = field(detail, read);
        move || {
            value
                .get()
                .flatten()
                .map(|value| view! { <dt>{label}</dt><dd>{value}</dd> })
        }
    };
    view! {
        <dl class="facts">
            {fact("Duration", |detail| Some(detail.duration.clone()))}
            {fact("Author", |detail| detail.author.clone())}
            {fact("Origin", |detail| detail.origin.clone())}
        </dl>
    }
}

#[component]
fn TrashButton(target: Target) -> impl IntoView {
    let error = new_error();
    view! {
        <div class="row-buttons">
            <button type="button" class="button"
                on:click=move |_| target.edit(error, edit::trash_event)>"Move to trash"</button>
        </div>
        {error_line(error)}
    }
}

/// Restore or purge an event in the trash; purging asks first.
#[component]
pub fn TrashActions(uuid: String, selected: RwSignal<Option<Row>>) -> impl IntoView {
    let target = Target::new(uuid, EVENT);
    let error = new_error();
    let purge = move |_| {
        if confirm("Delete this event for good? This cannot be undone.") {
            target.edit(error, edit::purge_event);
            if error.with_untracked(Option::is_none) {
                selected.set(None);
            }
        }
    };
    view! {
        <div class="row-buttons">
            <button type="button" class="button primary"
                on:click=move |_| target.edit(error, edit::restore_event)>"Restore"</button>
            <button type="button" class="button danger" on:click=purge>"Delete for good"</button>
        </div>
        {error_line(error)}
    }
}
