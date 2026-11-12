//! The selected event: its editor for those who can write, else what it holds.

use leptos::prelude::*;

use crate::detail::{EventDetail, event_detail, range_text};
use crate::format::duration_text;
use crate::route::Route;
use crate::rows::Row;
use crate::state::Room;
use crate::views::catalogue::OnScreen;
use crate::views::catalogue_details::CatalogueDetails;
use crate::views::event_editor::{Editor, TrashActions};
use crate::views::history::LastChanges;
use crate::views::stats::Statistics;

/// What the panel shows. Built from the selection so that the editor of one event stays put
/// while the room changes around it.
#[derive(Clone, PartialEq)]
pub enum Shown {
    Nothing,
    Derived(Row),
    Event(String),
}

impl Shown {
    pub fn of(selected: &Option<Row>) -> Self {
        match selected {
            None => Shown::Nothing,
            Some(row) => match &row.uuid {
                Some(uuid) => Shown::Event(uuid.clone()),
                None => Shown::Derived(row.clone()),
            },
        }
    }

    pub fn event(&self) -> Option<&str> {
        match self {
            Shown::Event(uuid) => Some(uuid),
            _ => None,
        }
    }
}

#[component]
pub fn EventPanel(
    selected: RwSignal<Option<Row>>,
    shown: Memo<Shown>,
    route: Memo<Route>,
    screen: RwSignal<Option<OnScreen>>,
) -> impl IntoView {
    let content = move || match shown.get() {
        Shown::Nothing => view! {
            <CatalogueDetails route/>
            <Statistics screen/>
        }
        .into_any(),
        Shown::Derived(row) => derived(&row).into_any(),
        Shown::Event(uuid) => view! {
            <Event uuid=uuid.clone() selected/>
            <LastChanges uuid shown=route/>
        }
        .into_any(),
    };
    view! {
        <aside class="panel" class:showing-event=move || shown.with(|shown| *shown != Shown::Nothing)
            aria-label=move || match shown.get() {
            Shown::Nothing => "Statistics",
            _ => "Selected event",
        }>
            <Show when=move || shown.with(|shown| *shown != Shown::Nothing)>
                <button type="button" class="close" aria-label="Close"
                    on:click=move |_| selected.set(None)>"×"</button>
            </Show>
            {content}
        </aside>
    }
}

/// The parts of the panel that depend on whether the event is there, in the trash, and
/// whether the visitor may write.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Gone,
    Read,
    Edit,
    Trashed,
}

#[component]
fn Event(uuid: String, selected: RwSignal<Option<Row>>) -> impl IntoView {
    let room = expect_context::<Room>();
    let can_write = room.can_write();
    let loading = room.loading;
    let detail = {
        let uuid = uuid.clone();
        Memo::new(move |_| room.read(|txn| event_detail(txn, &uuid)))
    };
    let mode = Memo::new(move |_| {
        detail.with(|detail| match detail {
            None => Mode::Gone,
            Some(_) if !can_write => Mode::Read,
            Some(detail) if detail.deleted => Mode::Trashed,
            Some(_) => Mode::Edit,
        })
    });
    move || match mode.get() {
        Mode::Gone if loading.with(Option::is_some) => {
            view! { <p class="quiet">"The event opens once the project has loaded."</p> }.into_any()
        }
        Mode::Gone => {
            view! { <p class="quiet">"This event is no longer in the project."</p> }.into_any()
        }
        Mode::Read => detail.get().map(read_only).into_any(),
        Mode::Trashed => view! {
            {detail.get().map(read_only)}
            <TrashActions uuid=uuid.clone() selected/>
        }
        .into_any(),
        Mode::Edit => view! { <Editor uuid=uuid.clone() detail/> }.into_any(),
    }
}

fn read_only(detail: EventDetail) -> impl IntoView {
    let field = |label: &'static str, value: Option<String>| {
        value.map(|value| view! { <dt>{label}</dt><dd>{value}</dd> })
    };
    view! {
        {head(&detail)}
        <dl class="facts">
            {field("Duration", Some(detail.duration.clone()))}
            {field("Status", Some(detail.status.clone()))}
            {field("Confidence", detail.confidence.map(|confidence| format!("{confidence:.2}")))}
            {field("Author", detail.author.clone())}
            {field("Origin", detail.origin.clone())}
        </dl>
        {(!detail.tags.is_empty()).then(|| view! {
            <div class="tags">
                {detail.tags.iter().map(|tag| view! { <span class="tag">{tag.clone()}</span> }).collect_view()}
            </div>
        })}
        {(!detail.attributes.is_empty()).then(|| view! {
            <section>
                <h3 class="section-label">"Attributes"</h3>
                <dl class="attributes">
                    {detail.attributes.iter().map(|(key, value)| view! {
                        <dt>{key.clone()}</dt><dd class="mono">{value.clone()}</dd>
                    }).collect_view()}
                </dl>
            </section>
        })}
        {detail.comment.clone().map(|comment| view! {
            <section>
                <h3 class="section-label">"Comment"</h3>
                <p class="comment">{comment}</p>
            </section>
        })}
        {(!detail.catalogues.is_empty()).then(|| view! {
            <section>
                <h3 class="section-label">"Listed in"</h3>
                <ul class="plain">
                    {detail.catalogues.iter().map(|named| view! { <li>{named.name.clone()}</li> }).collect_view()}
                </ul>
            </section>
        })}
    }
}

pub fn head(detail: &EventDetail) -> impl IntoView + use<> {
    let range = match &detail.error {
        Some(error) => format!("→ {} · {error}", detail.stop),
        None => format!("→ {}", detail.stop),
    };
    let eyebrow = if detail.deleted {
        "Event in the trash"
    } else {
        "Event"
    };
    view! {
        <div class="panel-head">
            <span class="eyebrow">{eyebrow}</span>
            <h2 class="mono">{detail.start.clone()}</h2>
            <p class="quiet mono">{range}</p>
        </div>
    }
}

fn derived(row: &Row) -> impl IntoView + use<> {
    let (start, stop) = range_text(row.start, row.stop);
    let count = row.sources.len();
    let plural = if count == 1 { "" } else { "s" };
    view! {
        <div class="panel-head">
            <span class="eyebrow">"Derived interval"</span>
            <h2 class="mono">{start}</h2>
            <p class="quiet mono">{format!("→ {stop}")}</p>
        </div>
        <dl class="facts">
            <dt>"Duration"</dt><dd>{duration_text(row.stop - row.start)}</dd>
            <dt>"Made from"</dt><dd>{format!("{count} event{plural}")}</dd>
        </dl>
        <p class="quiet">"The rule made this interval; it is not an event of the project."</p>
    }
}
