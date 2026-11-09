//! Menus are `<details data-menu>`: a press outside an open one, or Escape, closes it.

use leptos::ev;
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{HtmlDetailsElement, Node};

/// Installs the closing rule once for the whole page.
pub fn close_menus_from_outside() {
    window_event_listener(ev::pointerdown, |event| {
        let pressed = event
            .target()
            .and_then(|target| target.dyn_into::<Node>().ok());
        close_open_menus(|menu| menu.contains(pressed.as_ref()));
    });
    window_event_listener(ev::keydown, |event| {
        if event.key() == "Escape" {
            close_open_menus(|_| false);
        }
    });
}

fn close_open_menus(keep: impl Fn(&HtmlDetailsElement) -> bool) {
    let Ok(open) = document().query_selector_all("details[data-menu][open]") else {
        return;
    };
    (0..open.length())
        .filter_map(|index| open.item(index)?.dyn_into::<HtmlDetailsElement>().ok())
        .filter(|menu| !keep(menu))
        .for_each(|menu| menu.set_open(false));
}
