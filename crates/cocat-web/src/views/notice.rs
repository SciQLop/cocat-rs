//! The server's word on the last action of an old page's form, shown once above the app.

use leptos::prelude::*;

use crate::views::invite::CopyLink;
use crate::views::projects::Projects;

#[derive(Debug, Clone)]
pub struct Notice {
    pub text: String,
    /// A share link the action made: the only time it is shown.
    pub link: Option<String>,
}

#[component]
pub fn NoticeBar() -> impl IntoView {
    let notice = expect_context::<Projects>().notice;
    move || {
        notice.get().map(|shown| {
            view! {
                <div class="notice-bar" role="status">
                    <div class="grow">
                        <p>{shown.text}</p>
                        {shown.link.map(|link| view! { <CopyLink link/> })}
                    </div>
                    <button type="button" class="icon" aria-label="Dismiss" on:click=move |_| notice.set(None)>"✕"</button>
                </div>
            }
        })
    }
}
