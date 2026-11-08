use leptos::prelude::*;

use crate::help;
use crate::presence::Peer;
use crate::route::Route;
use crate::state::Room;
use crate::sync::Status;
use crate::views::projects::ProjectMenu;
use crate::views::share::ShareButton;

#[component]
pub fn Header(shown: Memo<Route>) -> impl IntoView {
    let room = expect_context::<Room>();
    let config = room.config.clone();
    let status = room.status;
    let read_only = !config.role.can_write();
    let public = cocat_core::room_id::is_public_id(&config.room_id);
    let page = window().location().pathname().unwrap_or_default();
    let in_history = move || matches!(shown.get(), Route::History { .. });
    let here = move || match shown.get() {
        Route::History { .. } => Some("History"),
        Route::Groups | Route::Group(_) => Some("Groups"),
        Route::Public => Some("Public catalogues"),
        _ => None,
    };
    let history = move || shown.get().history().href(&page);
    view! {
        <header class="app-header">
            <a class="brand" href="../app">
                <img src="mark.png" alt="" width="24" height="28"/>
                "cocat"
            </a>
            <nav class="crumbs" aria-label="Breadcrumb">
                <ProjectMenu/>
                {move || here().map(|here| view! {
                    <span aria-hidden="true">"/"</span>
                    <span class="here" aria-current="page">{here}</span>
                })}
            </nav>
            <div class="header-end">
                <Avatars/>
                <Show when=move || public>
                    <span class="chip public-badge" title="A public catalogue: everyone signed in reads it, curators edit it">"Public"</span>
                </Show>
                <Show when=move || !public>
                    <ShareButton/>
                </Show>
                <Show when=move || !in_history()>
                    <a class="button history-button" href=history.clone()>"History"</a>
                </Show>
                <a class="button help-button" href=help::PAGE target="_blank" rel="noopener">"Help"</a>
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
        format!("{count} other {plural} in this project")
    };
    let title = move || {
        format!(
            "{}. A row in someone's colour is the event they are editing.",
            label()
        )
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
            <div class="avatars" role="group" aria-label=label title=title>
                <For each=move || peers.get() key=|peer| peer.email.clone() children=avatar/>
            </div>
        </Show>
    }
}
