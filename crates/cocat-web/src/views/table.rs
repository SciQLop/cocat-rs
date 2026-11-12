//! The event table. Only the rows in view, and a few beyond, are in the page: the rest
//! are two spacers, so a catalogue of any size scrolls at the same cost.

use std::collections::HashMap;

use leptos::html::Div;
use leptos::prelude::*;

use crate::format::{duration_text, person_text, time_text, value_text};
use crate::layout::{tags_that_fit, window};
use crate::presence::{Peer, editing};
use crate::preview::sort_hint;
use crate::rows::{Row, SortKey};
use crate::state::Room;
use crate::views::catalogue::Members;

/// Every row's height in pixels; the stylesheet sets the same.
const ROW: f64 = 40.0;
/// The table's narrowest width: the stylesheet's fixed columns, the author column at 100 px
/// (it takes whatever room is left), and 130 px per attribute column.
const FIXED_WIDTH: f64 = 830.0;
const ATTRIBUTE_WIDTH: f64 = 130.0;
/// The tag cell's inner width: `.w-tags` less the cell's padding.
const TAGS_WIDTH: f64 = 162.0;

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
    empty: Signal<&'static str>,
    /// A link under the empty text, when the page has somewhere to send the visitor.
    #[prop(optional)]
    empty_link: Option<Signal<Option<(&'static str, String)>>>,
    /// Where the table is scrolled to; the table opens there.
    scroll_top: RwSignal<f64>,
    /// The preview cannot sort yet.
    #[prop(default = true)]
    sortable: bool,
    /// Told where the view ends, as a position in `order`.
    #[prop(optional)]
    view_end: Option<RwSignal<usize>>,
) -> impl IntoView {
    let peers = expect_context::<Room>().peers;
    let editors = Memo::new(move |_| peers.with(|peers| editing(peers)));
    let scroller = NodeRef::<Div>::new();
    let viewport = RwSignal::new(ROW * 16.0);
    let measure = move || {
        if let Some(scroller) = scroller.get() {
            scroll_top.set(f64::from(scroller.scroll_top()));
            viewport.set(f64::from(scroller.client_height()));
        }
    };
    let opening_at = scroll_top.get_untracked();
    Effect::new(move |_| {
        if let Some(scroller) = scroller.get() {
            scroller.set_scroll_top(opening_at as i32);
        }
        measure();
    });
    let range =
        Memo::new(move |_| window(scroll_top.get(), viewport.get(), ROW, order.with(Vec::len)));
    if let Some(view_end) = view_end {
        Effect::new(move |_| view_end.set(range.get().end));
    }
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
                    .map(|&index| row(&rows[index], &columns, selected, editors))
                    .collect::<Vec<_>>()
            })
        });
        view! {
            <tr class="spacer" style=format!("height: {}px", range.start as f64 * ROW)></tr>
            {rows}
            <tr class="spacer" style=format!("height: {}px", (total - range.end) as f64 * ROW)></tr>
        }
    };
    let none_shown = move || order.with(Vec::is_empty);
    view! {
        <div class="table-scroll" node_ref=scroller on:scroll=move |_| measure()>
            <table class="events" style=move || {
                let width = FIXED_WIDTH + ATTRIBUTE_WIDTH * columns.with(Vec::len) as f64;
                format!("min-width: {width}px")
            }>
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
                        {FIXED.into_iter().map(|(label, key)| header(label, key, sorting, sortable)).collect_view()}
                        {move || columns.get().into_iter().map(|key| {
                            header_owned(key.clone(), SortKey::Attribute(key), sorting, sortable)
                        }).collect_view()}
                    </tr>
                </thead>
                <tbody>{body}</tbody>
            </table>
            <Show when=none_shown>
                <p class="quiet empty">{empty}</p>
                {move || empty_link.and_then(|link| link.get()).map(|(label, href)| view! {
                    <p class="empty-link"><a class="button" href=href>{label}</a></p>
                })}
            </Show>
        </div>
    }
}

fn header(
    label: &'static str,
    key: SortKey,
    sorting: RwSignal<(SortKey, bool)>,
    sortable: bool,
) -> impl IntoView {
    header_owned(label.to_owned(), key, sorting, sortable)
}

fn header_owned(
    label: String,
    key: SortKey,
    sorting: RwSignal<(SortKey, bool)>,
    sortable: bool,
) -> impl IntoView {
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
        <th scope="col" aria-sort=sorted_by title=sort_hint(sortable)>
            <button type="button" on:click=pick disabled=!sortable>{label}</button>
        </th>
    }
}

fn row(
    row: &Row,
    columns: &[String],
    selected: RwSignal<Option<Row>>,
    editors: Memo<HashMap<String, Peer>>,
) -> impl IntoView + use<> {
    let editor = {
        let uuid = row.uuid.clone();
        move || {
            let uuid = uuid.as_deref()?;
            editors.with(|editors| editors.get(uuid).cloned())
        }
    };
    let editor = Memo::new(move |_| editor());
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
            class:peer=move || editor.with(Option::is_some)
            style=move || editor.with(|peer| peer.as_ref().map(|peer| format!("--peer: {}", peer.color)))
            title=move || editor.with(|peer| peer.as_ref().map(|peer| format!("{} is editing this event", peer.email)))
            on:click=move |_| selected.set(Some(picked.clone()))>
            <td class="mono">{time_text(row.start)}</td>
            <td class="mono">{duration_text(row.stop - row.start)}</td>
            <td class="text">{tags(row)}</td>
            <td>{confidence(row.confidence)}</td>
            <td>{status(row)}</td>
            <td class="text quiet-cell" title=row.author.clone()>{row.author.as_deref().map(person_text).unwrap_or_default().to_owned()}</td>
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
    let shown = tags_that_fit(&row.tags, TAGS_WIDTH);
    let hidden = row.tags.len() - shown;
    view! {
        <span class="tags" title=(hidden > 0).then(|| row.tags.join(", "))>
            {row.tags[..shown].iter().map(|tag| view! { <span class="tag">{tag.clone()}</span> }).collect_view()}
            {(hidden > 0).then(|| view! { <span class="tag more">{format!("+{hidden}")}</span> })}
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
