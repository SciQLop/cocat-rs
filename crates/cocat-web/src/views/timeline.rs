//! The strip of where the events on screen lie in time, and the period picked on it.

use leptos::html::Div;
use leptos::prelude::*;
use web_sys::PointerEvent;

use crate::layout::{Span, spans, ticks};
use crate::period::{Extent, Grip, Period, Tap, counts_text};
use crate::views::catalogue::Members;

/// Slices of the strip: about two pixels each at desktop width.
const BUCKETS: usize = 400;
const TICKS: usize = 4;
/// How far from a handle, in pixels, a press still grabs it.
const MOUSE_REACH: f64 = 8.0;
const TOUCH_REACH: f64 = 18.0;

#[derive(Clone, PartialEq, Default)]
struct Strip {
    spans: Vec<Span>,
    ticks: Vec<String>,
    extent: Option<Extent>,
}

#[component]
pub fn Timeline(
    members: Members,
    matching: Memo<Vec<usize>>,
    period: RwSignal<Option<Period>>,
) -> impl IntoView {
    let strip = Memo::new(move |_| {
        members.with(|members| {
            let Ok(rows) = members else {
                return Strip::default();
            };
            let mut ranges: Vec<(i64, i64)> = matching.with(|matching| {
                matching
                    .iter()
                    .map(|&index| (rows[index].start, rows[index].stop))
                    .collect()
            });
            ranges.sort_unstable();
            let extent = Extent::of(&ranges);
            Strip {
                spans: spans(&ranges, BUCKETS),
                ticks: extent
                    .map(|extent| ticks(extent.first, extent.last, TICKS))
                    .unwrap_or_default(),
                extent,
            }
        })
    });
    let extent = Memo::new(move |_| strip.with(|strip| strip.extent));
    view! {
        <section class="timeline" aria-label="Timeline" tabindex="-1"
            on:keydown=move |event| if event.key() == "Escape" { period.set(None); }>
            <Brush extent period>
                {move || strip.with(|strip| strip.spans.iter().map(bar).collect_view())}
            </Brush>
            <div class="axis">
                {move || strip.with(|strip| {
                    strip.ticks.iter().map(|tick| view! { <span>{tick.clone()}</span> }).collect_view()
                })}
            </div>
        </section>
    }
}

/// The strip as a surface to drag a period on: press and drag to pick one, drag a handle
/// or the band to change it, double-tap to clear it.
#[component]
fn Brush(
    extent: Memo<Option<Extent>>,
    period: RwSignal<Option<Period>>,
    children: Children,
) -> impl IntoView {
    let node = NodeRef::<Div>::new();
    let grip = StoredValue::new(None::<Grip>);
    let last_tap = StoredValue::new(None::<Tap>);
    let pointer = move |event: &PointerEvent| {
        let rect = node.get_untracked()?.get_bounding_client_rect();
        let extent = extent.get_untracked()?;
        Some((
            extent,
            f64::from(event.client_x()) - rect.left(),
            rect.width(),
        ))
    };
    let press = move |event: PointerEvent| {
        let Some((extent, x, width)) = pointer(&event) else {
            return;
        };
        let tap = Tap {
            at: event.time_stamp(),
            x,
        };
        if tap.doubles(last_tap.get_value()) {
            last_tap.set_value(None);
            grip.set_value(None);
            period.set(None);
            return;
        }
        last_tap.set_value(Some(tap));
        let reach = if event.pointer_type() == "touch" {
            TOUCH_REACH
        } else {
            MOUSE_REACH
        };
        grip.set_value(Some(Grip::at(
            period.get_untracked(),
            &extent,
            x,
            width,
            reach,
        )));
        if let Some(node) = node.get_untracked() {
            // Without the capture, the drag still follows the pointer while it is over the strip.
            let _ = node.set_pointer_capture(event.pointer_id());
        }
    };
    let drag = move |event: PointerEvent| {
        let (Some(held), Some((extent, x, width))) = (grip.get_value(), pointer(&event)) else {
            return;
        };
        let picked = held.drag(extent.time_at(x, width), &extent);
        if !picked.is_empty() {
            period.set(Some(picked));
        }
    };
    let release = move |_: PointerEvent| grip.set_value(None);
    let band = move || {
        let (extent, period) = (extent.get()?, period.get()?);
        let left = extent.percent_of(period.start);
        let width = extent.percent_of(period.stop) - left;
        let style = format!("left: {left:.3}%; width: {width:.3}%");
        Some(view! {
            <div class="band" role="group" tabindex="0" style=style
                aria-label=format!("Selected period {}. Press Escape to clear it.", period.text())>
                <span class="handle start" aria-hidden="true"></span>
                <span class="handle stop" aria-hidden="true"></span>
            </div>
        })
    };
    view! {
        <div class="strip" node_ref=node
            on:pointerdown=press on:pointermove=drag on:pointerup=release on:pointercancel=release>
            {children()}
            {band}
        </div>
    }
}

/// The period's dates and how many events it keeps, above the table.
#[component]
pub fn PeriodLine(
    period: RwSignal<Option<Period>>,
    order: Memo<Vec<usize>>,
    matching: Memo<Vec<usize>>,
) -> impl IntoView {
    let text = move || {
        period
            .get()
            .map(|period| counts_text(&period, order.with(Vec::len), matching.with(Vec::len)))
    };
    view! {
        <Show when=move || period.with(Option::is_some)>
            <p class="period-line" role="status">
                <span>{text}</span>
                <button type="button" class="link-button" on:click=move |_| period.set(None)>"✕ Clear"</button>
            </p>
        </Show>
    }
}

fn bar(span: &Span) -> impl IntoView + use<> {
    let style = format!("left: {:.3}%; width: {:.3}%", span.left, span.width);
    view! { <span class="bar" class:lane-b=span.lane == 1 style=style></span> }
}
