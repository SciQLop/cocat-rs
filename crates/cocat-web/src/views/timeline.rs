//! The strip of where the events on screen lie in time.

use leptos::prelude::*;

use crate::layout::{Span, spans, ticks};
use crate::views::catalogue::Members;

/// Slices of the strip: about two pixels each at desktop width.
const BUCKETS: usize = 400;
const TICKS: usize = 4;

#[derive(Clone, PartialEq, Default)]
struct Strip {
    spans: Vec<Span>,
    ticks: Vec<String>,
}

#[component]
pub fn Timeline(members: Members, order: Memo<Vec<usize>>) -> impl IntoView {
    let strip = Memo::new(move |_| {
        members.with(|members| {
            let Ok(rows) = members else {
                return Strip::default();
            };
            let mut ranges: Vec<(i64, i64)> = order.with(|order| {
                order
                    .iter()
                    .map(|&index| (rows[index].start, rows[index].stop))
                    .collect()
            });
            ranges.sort_unstable();
            let first = ranges.iter().map(|range| range.0).min();
            let last = ranges.iter().map(|range| range.1).max();
            Strip {
                spans: spans(&ranges, BUCKETS),
                ticks: first
                    .zip(last)
                    .map(|(first, last)| ticks(first, last, TICKS))
                    .unwrap_or_default(),
            }
        })
    });
    view! {
        <section class="timeline" aria-label="Timeline">
            <div class="strip">
                {move || strip.with(|strip| strip.spans.iter().map(bar).collect_view())}
            </div>
            <div class="axis">
                {move || strip.with(|strip| {
                    strip.ticks.iter().map(|tick| view! { <span>{tick.clone()}</span> }).collect_view()
                })}
            </div>
        </section>
    }
}

fn bar(span: &Span) -> impl IntoView + use<> {
    let style = format!("left: {:.3}%; width: {:.3}%", span.left, span.width);
    view! { <span class="bar" class:lane-b=span.lane == 1 style=style></span> }
}
