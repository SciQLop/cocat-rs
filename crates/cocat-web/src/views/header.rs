use leptos::prelude::*;

use crate::state::Room;
use crate::sync::Status;

#[component]
pub fn Header() -> impl IntoView {
    let room = expect_context::<Room>();
    let config = room.config.clone();
    let status = room.status;
    let read_only = !config.role.can_write();
    view! {
        <header class="app-header">
            <a class="brand" href="../rooms">
                <img src="mark.png" alt="" width="24" height="28"/>
                "cocat"
            </a>
            <nav class="crumbs" aria-label="Breadcrumb">
                <a href="../rooms">"My rooms"</a>
                <span aria-hidden="true">"/"</span>
                <a class="here" href=format!("../rooms/{}", config.room_id)>
                    {config.room_name.clone()}
                </a>
            </nav>
            <div class="header-end">
                <Show when=move || read_only>
                    <span class="chip">"Read-only"</span>
                </Show>
                <span
                    class="status"
                    class:live=move || status.get() == Status::Live
                    role="status"
                >
                    <span class="dot" aria-hidden="true"></span>
                    {move || status.get().label()}
                </span>
            </div>
        </header>
    }
}
