//! What the events on screen add up to, in the panel while no event is open.

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlDetailsElement;

use crate::format::{duration_text, grouped};
use crate::period::Period;
use crate::stats::{
    self, CatalogueStats, Counted, LOG_TICKS, Spread, Values, log_position, number_text,
    percent_text,
};
use crate::views::catalogue::OnScreen;
use crate::views::controls::Hint;

/// Where the open sections are remembered, for this visitor only.
const OPEN_KEY: &str = "cocat.stats.open";

#[component]
pub fn Statistics(screen: RwSignal<Option<OnScreen>>) -> impl IntoView {
    move || screen.get().map(|on_screen| view! { <Summary on_screen/> })
}

#[component]
fn Summary(on_screen: OnScreen) -> impl IntoView {
    // No heading of its own: the line under the page's title already counts what is shown.
    let counted = Memo::new(move |_| on_screen.with(|rows, _| stats::summary(rows)));
    let body = move || {
        counted.with(|stats| match stats.count {
            0 => view! { <p class="quiet">"Statistics show once the table lists events."</p> }
                .into_any(),
            _ => summary_view(stats).into_any(),
        })
    };
    view! {
        <div class="panel-head">
            <span class="eyebrow">"Statistics"</span>
        </div>
        {body}
        <Show when=move || counted.with(|stats| stats.count > 0)>
            <More on_screen/>
            <Hint anchor="statistics" topic="statistics"
                text="Coverage counts overlapping time once; gaps are the waits between events."/>
        </Show>
    }
}

fn summary_view(stats: &CatalogueStats) -> impl IntoView + use<> {
    let span = stats
        .span
        .map(|(first, last)| Period::between(first, last).text());
    let pairs = stats.overlapping_pairs as usize;
    let percent = stats
        .coverage_percent()
        .map(|percent| format!("{} of the span", percent_text(percent)));
    // Overlaps are often duplicates or a list merged twice: a line of its own, not a footnote.
    let overlaps = (pairs > 0).then(|| {
        let what = if pairs == 1 {
            "pair of events overlaps"
        } else {
            "pairs of events overlap"
        };
        format!("{} {what}", grouped(pairs))
    });
    let span = span.map(|text| {
        text.split(" → ")
            .map(|end| view! { <span class="nowrap">{end.to_owned()}</span> })
            .collect::<Vec<_>>()
    });
    view! {
        <dl class="facts stat-facts">
            <dt>"Span"</dt>
            <dd>{span.map(|mut ends| {
                let last = ends.pop();
                view! { {ends}" → "{last} }
            })}</dd>
            <dt>"Coverage"</dt>
            <dd>
                {format!("{} covered", duration_text(stats.covered))}
                {percent.map(|percent| view! { <span class="stat-note">{percent}</span> })}
                {overlaps.map(|overlaps| view! { <span class="stat-warning">{overlaps}</span> })}
            </dd>
            <dt>"Duration"</dt><dd>{stats.duration.map(spread_view)}</dd>
        </dl>
        <Histogram bins=stats.durations.clone() ticks=log_ticks() label="Durations"/>
        <section>
            <h3 class="section-label">"Event tags"</h3>
            <Shares counted=stats.tags.clone() total=stats.count missing="untagged"/>
        </section>
    }
}

fn spread_view(spread: Spread) -> impl IntoView {
    let range = format!(
        "from {} to {}",
        duration_text(spread.min),
        duration_text(spread.max)
    );
    view! {
        {format!("median {}", duration_text(spread.median))}
        <span class="stat-note">{range}</span>
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn log_ticks() -> Vec<(String, f64)> {
    LOG_TICKS
        .iter()
        .map(|&(label, nanos)| (label.to_owned(), log_position(nanos)))
        .collect()
}

/// One bar per value, its length the share of the events that have it.
#[component]
fn Shares(counted: Counted, total: usize, missing: &'static str) -> impl IntoView {
    let percent = move |count: usize| count as f64 / total.max(1) as f64 * 100.0;
    let rows = counted
        .shares
        .into_iter()
        .map(|share| {
            let share_percent = percent(share.count);
            view! {
                <span class="stat-name" title=share.name.clone()>{share.name.clone()}</span>
                <span class="stat-track"><span style=format!("width: {share_percent:.1}%")></span></span>
                <span class="stat-num">{format!("{} · {}", share.count, percent_text(share_percent))}</span>
            }
        })
        .collect_view();
    view! {
        <div class="stat-bars">{rows}</div>
        {(counted.missing > 0).then(|| view! {
            <p class="stat-note">{format!("{} {missing}", counted.missing)}</p>
        })}
    }
}

/// Bars over fixed bins, scaled to the fullest, with labels placed along the axis.
#[component]
fn Histogram(bins: Vec<u32>, ticks: Vec<(String, f64)>, label: &'static str) -> impl IntoView {
    let fullest = bins.iter().copied().max().unwrap_or(0).max(1);
    let bars = bins
        .into_iter()
        .map(|count| {
            let height = f64::from(count) / f64::from(fullest) * 100.0;
            let title = format!(
                "{} event{}",
                grouped(count as usize),
                plural(count as usize)
            );
            view! { <span title=title style=format!("height: {height:.1}%")></span> }
        })
        .collect_view();
    let ticks = ticks
        .into_iter()
        .map(|(text, at)| {
            let edge = match at {
                0.0 => "start",
                100.0 => "end",
                _ => "",
            };
            view! { <span class=edge style=format!("left: {at:.1}%")>{text}</span> }
        })
        .collect_view();
    view! {
        <div class="histogram" role="img" aria-label=label>
            <div class="histogram-bars">{bars}</div>
            <div class="histogram-axis">{ticks}</div>
        </div>
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Section {
    Gaps,
    Status,
    Attributes,
    Tags,
    Authors,
}

impl Section {
    const ALL: [Section; 5] = [
        Section::Gaps,
        Section::Status,
        Section::Attributes,
        Section::Tags,
        Section::Authors,
    ];

    fn id(self) -> &'static str {
        match self {
            Section::Gaps => "gaps",
            Section::Status => "status",
            Section::Attributes => "attributes",
            Section::Tags => "tags",
            Section::Authors => "authors",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Section::Gaps => "Gaps between events",
            Section::Status => "Status and confidence",
            Section::Attributes => "Event attributes",
            Section::Tags => "All event tags",
            Section::Authors => "Authors",
        }
    }

    fn body(self, on_screen: OnScreen) -> AnyView {
        match self {
            Section::Gaps => on_screen.with(|rows, _| gaps_view(stats::gaps(rows))).into_any(),
            Section::Status => on_screen
                .with(|rows, _| statuses_view(stats::statuses(rows), rows.len()))
                .into_any(),
            Section::Attributes => on_screen
                .with(|rows, _| attributes_view(stats::attributes(rows)))
                .into_any(),
            Section::Tags => on_screen
                .with(|rows, _| tags_view(stats::all_tags(rows), rows.len()))
                .into_any(),
            Section::Authors => on_screen
                .with(|rows, _| view! { <Shares counted=stats::authors(rows) total=rows.len() missing="without an author"/> })
                .into_any(),
        }
    }
}

/// The extra sections, closed until asked for. A closed section computes nothing.
#[component]
fn More(on_screen: OnScreen) -> impl IntoView {
    let open = RwSignal::new(remembered());
    Effect::new(move |_| open.with(|open| remember(open)));
    let sections = Section::ALL
        .into_iter()
        .map(|section| {
            let id = section.id();
            let is_open = move || open.with(|open| open.iter().any(|known| known == id));
            let toggled = move |event: leptos::ev::Event| {
                let Some(details) = event
                    .target()
                    .and_then(|target| target.dyn_into::<HtmlDetailsElement>().ok())
                else {
                    return;
                };
                let now = details.open();
                if now != is_open() {
                    open.update(|open| match now {
                        true => open.push(id.to_owned()),
                        false => open.retain(|known| known != id),
                    });
                }
            };
            view! {
                <details class="stat-more" data-section=id prop:open=is_open on:toggle=toggled>
                    <summary>{section.title()}</summary>
                    {move || is_open().then(|| section.body(on_screen))}
                </details>
            }
        })
        .collect_view();
    view! {
        <section class="stat-sections">
            <h3 class="section-label">"More statistics"</h3>
            {sections}
        </section>
    }
}

fn storage() -> Option<web_sys::Storage> {
    window().local_storage().ok().flatten()
}

fn remembered() -> Vec<String> {
    storage()
        .and_then(|storage| storage.get_item(OPEN_KEY).ok().flatten())
        .map(|text| {
            text.split(',')
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The page works the same without storage; the choice is then forgotten on reload.
fn remember(open: &[String]) {
    if let Some(storage) = storage() {
        let _ = storage.set_item(OPEN_KEY, &open.join(","));
    }
}

fn gaps_view(gaps: stats::Gaps) -> impl IntoView {
    match gaps.spread {
        None => {
            view! { <p class="stat-note">"No gaps: the events overlap or there is only one."</p> }
                .into_any()
        }
        Some(spread) => view! {
            <p class="stat-line">{spread_view(spread)}</p>
            <Histogram bins=gaps.bins ticks=log_ticks() label="Gaps between events"/>
        }
        .into_any(),
    }
}

fn statuses_view(statuses: stats::Statuses, total: usize) -> impl IntoView {
    let width = move |count: usize| count as f64 / total.max(1) as f64 * 100.0;
    let segments = statuses
        .counts
        .iter()
        .map(|&(status, count)| {
            view! {
                <span class=format!("stat-tone {}", status.name()) title=format!("{}: {count}", status.name())
                    style=format!("width: {:.2}%", width(count))></span>
            }
        })
        .collect_view();
    let legend = statuses
        .counts
        .iter()
        .map(|&(status, count)| {
            view! {
                <li><span class=format!("stat-swatch stat-tone {}", status.name())></span>
                    {status.name()} " " <span class="stat-num">{count}</span></li>
            }
        })
        .collect_view();
    let ends = vec![("0".to_owned(), 0.0), ("1".to_owned(), 100.0)];
    view! {
        <div class="stat-stack" role="img" aria-label="Events by status">{segments}</div>
        <ul class="stat-legend">{legend}</ul>
        {statuses.confidence_mean.map(|mean| view! {
            <p class="stat-line">{format!("Confidence: mean {}", number_text(mean))}</p>
            <Histogram bins=statuses.confidence_bins.clone() ticks=ends.clone() label="Confidence from 0 to 1"/>
        })}
        {(statuses.without_confidence > 0).then(|| view! {
            <p class="stat-note">{format!("{} without confidence", statuses.without_confidence)}</p>
        })}
    }
}

fn attributes_view(attributes: Vec<stats::AttributeStats>) -> impl IntoView {
    if attributes.is_empty() {
        return view! { <p class="stat-note">"No event has attributes."</p> }.into_any();
    }
    let rows = attributes
        .into_iter()
        .map(|attribute| {
            let values = match attribute.values {
                Values::Numbers { min, median, max } => format!(
                    "{} to {}, median {}",
                    number_text(min),
                    number_text(max),
                    number_text(median)
                ),
                Values::Text { distinct, top } => {
                    let top: Vec<String> = top
                        .iter()
                        .map(|share| format!("{} ({})", share.name, share.count))
                        .collect();
                    format!("{distinct} value{}: {}", plural(distinct), top.join(", "))
                }
            };
            view! {
                <dt class="mono">{attribute.key}</dt>
                <dd>
                    <span class="stat-num">{format!("{} event{}", attribute.present, plural(attribute.present))}</span>
                    " · " {values}
                </dd>
            }
        })
        .collect_view();
    view! { <dl class="attributes stat-attributes">{rows}</dl> }.into_any()
}

fn tags_view(tags: stats::TagStats, total: usize) -> impl IntoView {
    let pairs = tags
        .pairs
        .iter()
        .map(|(first, second, count)| {
            view! { <li>{format!("{first} + {second}")} " " <span class="stat-num">{*count}</span></li> }
        })
        .collect_view();
    let has_pairs = !tags.pairs.is_empty();
    view! {
        <Shares counted=tags.tags total missing="untagged"/>
        {has_pairs.then(|| view! {
            <h4 class="section-label">"Often together"</h4>
            <ul class="plain stat-pairs">{pairs}</ul>
        })}
    }
}
