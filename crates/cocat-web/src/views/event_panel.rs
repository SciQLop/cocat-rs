//! The selected event, read-only for now.

use leptos::prelude::*;

use crate::detail::{EventDetail, event_detail, range_text};
use crate::format::duration_text;
use crate::rows::Row;
use crate::state::Room;

#[component]
pub fn EventPanel(selected: RwSignal<Option<Row>>) -> impl IntoView {
    let room = expect_context::<Room>();
    let detail = Memo::new(move |_| {
        let uuid = selected.with(|selected| selected.as_ref().and_then(|row| row.uuid.clone()))?;
        Some(room.read(|txn| event_detail(txn, &uuid)))
    });
    let content = move || match (selected.get(), detail.get()) {
        (None, _) => view! { <p class="quiet">"Pick an event to see it here."</p> }.into_any(),
        (Some(_), Some(None)) => {
            view! { <p class="quiet">"This event is no longer in the room."</p> }.into_any()
        }
        (Some(_), Some(Some(detail))) => event(detail).into_any(),
        (Some(row), None) => derived(&row).into_any(),
    };
    view! {
        <aside class="panel" aria-label="Selected event">
            <Show when=move || selected.with(Option::is_some)>
                <button type="button" class="close" aria-label="Close"
                    on:click=move |_| selected.set(None)>"×"</button>
            </Show>
            {content}
        </aside>
    }
}

fn event(detail: EventDetail) -> impl IntoView {
    let range = match &detail.error {
        Some(error) => format!("→ {} · {error}", detail.stop),
        None => format!("→ {}", detail.stop),
    };
    let field = |label: &'static str, value: Option<String>| {
        value.map(|value| view! { <dt>{label}</dt><dd>{value}</dd> })
    };
    view! {
        <div class="panel-head">
            <span class="eyebrow">"Event"</span>
            <h2 class="mono">{detail.start.clone()}</h2>
            <p class="quiet mono">{range}</p>
        </div>
        <dl class="facts">
            {field("Duration", Some(detail.duration.clone()))}
            {field("Status", Some(detail.status.clone()))}
            {field("Confidence", detail.confidence.clone())}
            {field("Author", detail.author.clone())}
            {field("Origin", detail.origin.clone())}
        </dl>
        <Show when={
            let empty = detail.tags.is_empty();
            move || !empty
        }>
            <div class="tags">
                {detail.tags.iter().map(|tag| view! { <span class="tag">{tag.clone()}</span> }).collect_view()}
            </div>
        </Show>
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
                    {detail.catalogues.iter().map(|name| view! { <li>{name.clone()}</li> }).collect_view()}
                </ul>
            </section>
        })}
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
        <p class="quiet">"The rule made this interval; it is not an event of the room."</p>
    }
}
