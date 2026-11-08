use leptos::prelude::*;

use crate::presence::Peer;
use crate::route::Route;
use crate::state::Room;
use crate::sync::Status;

#[component]
pub fn Header(shown: Memo<Route>) -> impl IntoView {
    let room = expect_context::<Room>();
    let config = room.config.clone();
    let status = room.status;
    let read_only = !config.role.can_write();
    let page = window().location().pathname().unwrap_or_default();
    let in_history = move || matches!(shown.get(), Route::History { .. });
    let history = move || shown.get().history().href(&page);
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
                <Show when=in_history>
                    <span aria-hidden="true">"/"</span>
                    <span class="here" aria-current="page">"History"</span>
                </Show>
            </nav>
            <div class="header-end">
                <Avatars/>
                <Show when=move || !in_history()>
                    <a class="button history-button" href=history.clone()>"History"</a>
                </Show>
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

/// The others in the room, each in the colour the server gave the account.
#[component]
fn Avatars() -> impl IntoView {
    let peers = expect_context::<Room>().peers;
    let label = move || {
        let count = peers.with(Vec::len);
        let plural = if count == 1 { "person" } else { "people" };
        format!("{count} other {plural} in this room")
    };
    let avatar = |peer: Peer| {
        view! {
            <span class="avatar" title=peer.email style=format!("background: {}", peer.color)>
                {peer.initials}
            </span>
        }
    };
    view! {
        <Show when=move || peers.with(|peers| !peers.is_empty())>
            <div class="avatars" role="group" aria-label=label>
                <For each=move || peers.get() key=|peer| peer.email.clone() children=avatar/>
            </div>
        </Show>
    }
}
