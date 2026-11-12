//! The handles that widen or narrow the side panes. Each width is a CSS variable on the
//! page, remembered in this browser.

use leptos::ev::{KeyboardEvent, PointerEvent};
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::layout::Side;

fn stored_as(side: Side) -> String {
    format!("cocat{}", side.key())
}

fn remembered(side: Side) -> f64 {
    window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|storage| storage.get_item(&stored_as(side)).ok().flatten())
        .and_then(|text| text.parse::<f64>().ok())
        .map_or(side.default_width(), |width| side.clamp(width))
}

fn remember(side: Side, width: f64) {
    if let Some(storage) = window().local_storage().ok().flatten() {
        let _ = storage.set_item(&stored_as(side), &width.to_string());
    }
}

fn apply(side: Side, width: f64) {
    if let Some(root) = document()
        .document_element()
        .and_then(|root| root.dyn_into::<web_sys::HtmlElement>().ok())
    {
        let _ = root.style().set_property(side.key(), &format!("{width}px"));
    }
}

/// Applies the widths remembered in this browser; once, as the page starts.
pub fn restore() {
    for side in [Side::Sidebar, Side::Panel] {
        apply(side, remembered(side));
    }
}

/// The edge of a side pane: drag it, or focus it and use the arrow keys; Home or a double
/// click goes back to the default width.
#[component]
pub fn Resizer(side: Side, label: &'static str) -> impl IntoView {
    let width = RwSignal::new(remembered(side));
    // Where the drag started: the pointer's x and the width then.
    let drag = StoredValue::new(None::<(f64, f64)>);
    let set = move |to: f64, keep: bool| {
        width.set(to);
        apply(side, to);
        if keep {
            remember(side, to);
        }
    };
    let down = move |event: PointerEvent| {
        event.prevent_default();
        if let Some(handle) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        {
            let _ = handle.set_pointer_capture(event.pointer_id());
        }
        drag.set_value(Some((f64::from(event.client_x()), width.get_untracked())));
    };
    let moved = move |event: PointerEvent| {
        if let Some((x, start)) = drag.get_value() {
            set(side.dragged(start, f64::from(event.client_x()) - x), false);
        }
    };
    let up = move |_: PointerEvent| {
        if drag.get_value().is_some() {
            drag.set_value(None);
            remember(side, width.get_untracked());
        }
    };
    let key = move |event: KeyboardEvent| {
        if let Some(to) = side.keyed(width.get_untracked(), &event.key()) {
            event.prevent_default();
            set(to, true);
        }
    };
    let (low, high) = side.bounds();
    view! {
        <div class="resizer" class:resizer-panel=side == Side::Panel role="separator"
            aria-orientation="vertical" tabindex="0" aria-label=label
            aria-valuemin=low aria-valuemax=high aria-valuenow=move || width.get().round()
            title="Drag to resize; double-click to reset"
            on:pointerdown=down on:pointermove=moved on:pointerup=up on:pointercancel=up
            on:keydown=key on:dblclick=move |_| set(side.default_width(), true)>
        </div>
    }
}
