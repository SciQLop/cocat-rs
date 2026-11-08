//! The event table. Only the rows in view, and a few beyond, are in the page: the rest
//! are two spacers, so a catalogue of any size scrolls at the same cost.

use leptos::html::Div;
use leptos::prelude::*;

use crate::format::{duration_text, time_text, value_text};
use crate::layout::window;
use crate::rows::{Row, SortKey};
use crate::views::catalogue::Members;

/// Every row's height in pixels; the stylesheet sets the same.
const ROW: f64 = 40.0;

const FIXED: [(&str, SortKey); 6] = [
    ("Start", SortKey::Start),
    ("Duration", SortKey::Duration),
    ("Tags", SortKey::Tags),
    ("Confidence", SortKey::Confidence),
    ("Status", SortKey::Status),
    ("Author", SortKey::Author),
];

#[component]
pub fn Table(
    members: Members,
    order: Memo<Vec<usize>>,
    sorting: RwSignal<(SortKey, bool)>,
    columns: Memo<Vec<String>>,
    selected: RwSignal<Option<Row>>,
) -> impl IntoView {
    let scroller = NodeRef::<Div>::new();
    let scroll_top = RwSignal::new(0.0);
    let viewport = RwSignal::new(ROW * 16.0);
    let measure = move || {
        if let Some(scroller) = scroller.get() {
            scroll_top.set(f64::from(scroller.scroll_top()));
            viewport.set(f64::from(scroller.client_height()));
        }
    };
    Effect::new(move |_| measure());
    let range =
        Memo::new(move |_| window(scroll_top.get(), viewport.get(), ROW, order.with(Vec::len)));
    let body = move || {
        let range = range.get();
        let total = order.with(Vec::len);
        let columns = columns.get();
        let rows = members.with(|members| {
            let Ok(rows) = members else {
                return Vec::new();
            };
            order.with(|order| {
                order[range.clone()]
                    .iter()
                    .map(|&index| row(&rows[index], &columns, selected))
                    .collect::<Vec<_>>()
            })
        });
        view! {
            <tr class="spacer" style=format!("height: {}px", range.start as f64 * ROW)></tr>
            {rows}
            <tr class="spacer" style=format!("height: {}px", (total - range.end) as f64 * ROW)></tr>
        }
    };
    let empty = move || order.with(Vec::is_empty);
    view! {
        <div class="table-scroll" node_ref=scroller on:scroll=move |_| measure()>
            <table class="events">
                <colgroup>
                    <col class="w-start"/>
                    <col class="w-duration"/>
                    <col class="w-tags"/>
                    <col class="w-confidence"/>
                    <col class="w-status"/>
                    <col class="w-author"/>
                    {move || columns.get().into_iter().map(|_| view! { <col class="w-attribute"/> }).collect_view()}
                </colgroup>
                <thead>
                    <tr>
                        {FIXED.into_iter().map(|(label, key)| header(label, key, sorting)).collect_view()}
                        {move || columns.get().into_iter().map(|key| {
                            header_owned(key.clone(), SortKey::Attribute(key), sorting)
                        }).collect_view()}
                    </tr>
                </thead>
                <tbody>{body}</tbody>
            </table>
            <Show when=empty>
                <p class="quiet empty">"No events to show."</p>
            </Show>
        </div>
    }
}

fn header(label: &'static str, key: SortKey, sorting: RwSignal<(SortKey, bool)>) -> impl IntoView {
    header_owned(label.to_owned(), key, sorting)
}

fn header_owned(label: String, key: SortKey, sorting: RwSignal<(SortKey, bool)>) -> impl IntoView {
    let sorted_by = {
        let key = key.clone();
        move || {
            sorting.with(
                |(current, descending)| match (current == &key, descending) {
                    (false, _) => "none",
                    (true, false) => "ascending",
                    (true, true) => "descending",
                },
            )
        }
    };
    let pick = move |_| {
        sorting.update(|(current, descending)| {
            *descending = *current == key && !*descending;
            *current = key.clone();
        });
    };
    view! {
        <th scope="col" aria-sort=sorted_by>
            <button type="button" on:click=pick>{label}</button>
        </th>
    }
}

fn row(row: &Row, columns: &[String], selected: RwSignal<Option<Row>>) -> impl IntoView + use<> {
    let is_selected = {
        let key = (row.uuid.clone(), row.start, row.stop);
        move || {
            selected.with(|selected| {
                selected
                    .as_ref()
                    .is_some_and(|row| (row.uuid.clone(), row.start, row.stop) == key)
            })
        }
    };
    let picked = row.clone();
    let attributes = columns
        .iter()
        .map(|key| {
            let text = row.attributes.get(key).map(value_text).unwrap_or_default();
            view! { <td class="text">{text}</td> }
        })
        .collect_view();
    view! {
        <tr class="row" aria-selected=move || is_selected().to_string()
            on:click=move |_| selected.set(Some(picked.clone()))>
            <td class="mono">{time_text(row.start)}</td>
            <td class="mono">{duration_text(row.stop - row.start)}</td>
            <td class="text">{tags(row)}</td>
            <td>{confidence(row.confidence)}</td>
            <td>{status(row)}</td>
            <td class="text quiet-cell">{row.author.clone().unwrap_or_default()}</td>
            {attributes}
        </tr>
    }
}

fn tags(row: &Row) -> impl IntoView + use<> {
    if row.uuid.is_none() {
        let count = row.sources.len();
        let plural = if count == 1 { "" } else { "s" };
        return view! { <span class="quiet-cell">{format!("from {count} event{plural}")}</span> }
            .into_any();
    }
    view! {
        <span class="tags">
            {row.tags.iter().map(|tag| view! { <span class="tag">{tag.clone()}</span> }).collect_view()}
        </span>
    }
    .into_any()
}

fn confidence(confidence: Option<f64>) -> impl IntoView + use<> {
    confidence.map(|confidence| {
        let width = format!("width: {:.0}%", confidence.clamp(0.0, 1.0) * 100.0);
        view! {
            <span class="confidence">
                <span class="meter"><span style=width></span></span>
                <span class="mono small">{format!("{confidence:.2}")}</span>
            </span>
        }
    })
}

fn status(row: &Row) -> impl IntoView + use<> {
    let name = if row.uuid.is_none() {
        "derived"
    } else {
        row.status.name()
    };
    view! { <span class=format!("badge {name}")>{name}</span> }
}
