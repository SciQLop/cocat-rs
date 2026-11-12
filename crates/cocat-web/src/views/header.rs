use leptos::prelude::*;

use leptos::html::Details;

use crate::help;
use crate::presence::{Peer, initials};
use crate::projects::shareable;
use crate::route::Route;
use crate::state::Room;
use crate::sync::Status;
use crate::views::projects::{ProjectActions, ProjectMenu, Projects};
use crate::views::share::ShareButton;

#[component]
pub fn Header(shown: Memo<Route>) -> impl IntoView {
    let room = expect_context::<Room>();
    let config = room.config.clone();
    let status = room.status;
    let read_only = !config.role.can_write();
    let projects = expect_context::<Projects>();
    let shareable = {
        let room_id = config.room_id.clone();
        move || projects.list.with(|listed| shareable(&room_id, listed))
    };
    let page = window().location().pathname().unwrap_or_default();
    let in_history = move || matches!(shown.get(), Route::History { .. });
    // A link shows another project's catalogue: this project's history would mislead.
    let on_link = move || matches!(shown.get(), Route::Link(_));
    let history = {
        let page = page.clone();
        move || shown.get().history().href(&page)
    };
    let close_history = move || shown.get().live().href(&page);
    view! {
        <header class="app-header">
            <Crumbs shown/>
            <div class="header-end">
                <Avatars/>
                <Show when=shareable>
                    <ShareButton/>
                </Show>
                <Show
                    when=move || !in_history()
                    fallback=move || view! {
                        <a class="button history-button" href=close_history.clone()>"Close history"</a>
                    }
                >
                    <a class="button history-button" href=history.clone() hidden=on_link>"History"</a>
                </Show>
                <HelpButton/>
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
                <UserMenu/>
            </div>
        </header>
    }
}

/// The header of the public catalogues' index and submissions: no project is open there, so
/// nothing to share, no history and no connection.
#[component]
pub fn PublicHeader(shown: Memo<Route>) -> impl IntoView {
    view! {
        <header class="app-header">
            <Crumbs shown/>
            <div class="header-end">
                <HelpButton/>
                <UserMenu/>
            </div>
        </header>
    }
}

/// The brand, the project switcher and where the page is after it.
#[component]
fn Crumbs(shown: Memo<Route>) -> impl IntoView {
    let page = window().location().pathname().unwrap_or_default();
    view! {
        <a class="brand" href=Route::Home.href(&page) title="Home">
            <img src="mark.png" alt="" width="24" height="28"/>
            "cocat"
        </a>
        <nav class="crumbs" aria-label="Breadcrumb">
            <ProjectMenu/>
            <ProjectActions/>
            {move || shown.get().crumb().map(|here| view! {
                <span aria-hidden="true">"/"</span>
                <span class="here" aria-current="page">{here}</span>
            })}
        </nav>
    }
}

#[component]
fn HelpButton() -> impl IntoView {
    view! { <a class="button help-button" href=help::PAGE target="_blank" rel="noopener">"Help"</a> }
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

/// The visitor's own avatar: their account, the admin pages for an admin, and signing out.
/// The links lead out of the app, to the server's pages beside it.
#[component]
fn UserMenu() -> impl IntoView {
    let (email, admin) = expect_context::<Projects>()
        .open
        .with_untracked(|config| (config.user_email.clone(), config.admin));
    let page = window().location().pathname().unwrap_or_default();
    let menu = NodeRef::<Details>::new();
    let close = move || {
        if let Some(menu) = menu.get_untracked() {
            menu.set_open(false);
        }
    };
    view! {
        <details class="user-menu" data-menu node_ref=menu
            on:keydown=move |event| if event.key() == "Escape" { close(); }>
            <summary class="avatar me" title=email.clone() aria-label="Your account"
                style=format!("background: {}", cocat_core::person::color(&email))>
                {initials(&email)}
            </summary>
            <div class="menu" role="menu" aria-label="Your account">
                <p class="signed-in">"Signed in as"<strong>{email.clone()}</strong></p>
                <hr/>
                <a role="menuitem" href="../account">"Account settings"</a>
                <a role="menuitem" href=Route::Groups.href(&page)>"Groups"</a>
                {admin.then(|| view! { <a role="menuitem" href="../admin">"Admin"</a> })}
                <hr/>
                <form method="post" action="../logout">
                    <button type="submit" role="menuitem">"Sign out"</button>
                </form>
            </div>
        </details>
    }
}
