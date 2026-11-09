//! What the Share dialog and a group's view share: inviting by email, making a share link,
//! copying it, and the pending invitations.

use leptos::html::Input;
use leptos::prelude::*;
use leptos::task::spawn_local;
use time::{Date, OffsetDateTime};
use wasm_bindgen::JsValue;

use crate::api::{self, ApiError};
use crate::sharing::{Created, Invitation, Target, invitation_link, invitation_text, invited_text};
use crate::views::controls::{Error, confirm, error_line};

/// Runs `request`; on success tells `changed`, else shows why in `error`.
pub fn act(
    error: Error,
    changed: Callback<()>,
    request: impl Future<Output = Result<(), ApiError>> + 'static,
) {
    error.set(None);
    spawn_local(async move {
        match request.await {
            Ok(()) => changed.run(()),
            Err(failure) => {
                error.try_set(Some(failure.to_string()));
            }
        }
    });
}

/// Editor or viewer, as the server assigns them.
#[component]
pub fn RolePicker(
    role: RwSignal<String>,
    #[prop(into)] label: String,
    #[prop(optional)] on_pick: Option<Callback<String>>,
) -> impl IntoView {
    let pick = move |event| {
        let picked = event_target_value(&event);
        role.set(picked.clone());
        if let Some(on_pick) = on_pick {
            on_pick.run(picked);
        }
    };
    view! {
        <select class=move || format!("role-picker role-{}", role.get()) aria-label=label on:change=pick prop:value=move || role.get()>
            <option value="editor">"editor"</option>
            <option value="viewer">"viewer"</option>
        </select>
    }
}

/// A role that can't be changed here, in the role's colour.
#[component]
pub fn RoleChip(#[prop(into)] role: String) -> impl IntoView {
    let class = format!("role role-chip role-{role}");
    view! { <span class=class>{role}</span> }
}

/// The link the server just made, shown once: it is the only time it exists in clear.
#[component]
pub fn CopyLink(link: String) -> impl IntoView {
    let input = NodeRef::<Input>::new();
    let said = RwSignal::new(None::<&'static str>);
    let copy = move |_| {
        let Some(input) = input.get_untracked() else {
            return;
        };
        input.select();
        let text = input.value();
        spawn_local(async move {
            let text = match write_clipboard(&text).await {
                true => "Copied.",
                false => "The link is selected: press Ctrl+C to copy it.",
            };
            said.try_set(Some(text));
        });
    };
    view! {
        <div class="copy-link">
            <input type="text" readonly node_ref=input value=link aria-label="Link to send"
                on:focus=move |_| { if let Some(input) = input.get_untracked() { input.select(); } }/>
            <button type="button" class="button" on:click=copy>"Copy"</button>
        </div>
        <p class="quiet small" role="status">
            {move || said.get().unwrap_or("Copy it now: it is shown only once.")}
        </p>
    }
}

/// The clipboard exists only on secure pages; elsewhere the selected text is the fallback.
async fn write_clipboard(text: &str) -> bool {
    let navigator = window().navigator();
    let present = js_sys::Reflect::get(&navigator, &JsValue::from_str("clipboard"))
        .is_ok_and(|clipboard| !clipboard.is_undefined());
    if !present {
        return false;
    }
    let written = navigator.clipboard().write_text(text);
    wasm_bindgen_futures::JsFuture::from(written).await.is_ok()
}

fn link_to(target: &Target, created: &Created) -> Option<String> {
    let base = document().base_uri().ok().flatten()?;
    invitation_link(&base, target.invitation_page(), &created.token)
}

/// An email address, its role for a project, and Invite. The server mails the link when it
/// can; otherwise the link is shown here to send by hand.
#[component]
pub fn InviteByEmail(
    target: Target,
    #[prop(into)] suggestions: Signal<Vec<String>>,
    changed: Callback<()>,
) -> impl IntoView {
    let input = NodeRef::<Input>::new();
    let role = RwSignal::new("editor".to_owned());
    let error: Error = RwSignal::new(None);
    let done = RwSignal::new(None::<(String, Option<String>)>);
    let roles = target.has_roles();
    let list = if roles {
        "people-project"
    } else {
        "people-group"
    };
    let submit = {
        let target = target.clone();
        move |event: leptos::ev::SubmitEvent| {
            event.prevent_default();
            let Some(field) = input.get_untracked() else {
                return;
            };
            let email = field.value().trim().to_owned();
            if email.is_empty() {
                return;
            }
            let target = target.clone();
            let role = roles.then(|| role.get_untracked());
            error.set(None);
            spawn_local(async move {
                let path = target.path(&["invitations"]);
                match api::invite(&target.api, &path, &email, role.as_deref()).await {
                    Ok(created) => {
                        let mailed = created.token.is_empty();
                        let link = (!mailed).then(|| link_to(&target, &created)).flatten();
                        done.try_set(Some((invited_text(&email, mailed), link)));
                        field.set_value("");
                        changed.run(());
                    }
                    Err(failure) => {
                        error.try_set(Some(failure.to_string()));
                    }
                }
            });
        }
    };
    view! {
        <form class="invite-row" on:submit=submit>
            <input type="email" required autofocus node_ref=input list=list autocomplete="off"
                placeholder="name@example.org" aria-label="Email to invite"
                on:input=move |_| error.set(None)/>
            <datalist id=list>
                <For each=move || suggestions.get() key=|email| email.clone() let(email)>
                    <option value=email></option>
                </For>
            </datalist>
            {roles.then(|| view! { <RolePicker role label="Role to give"/> })}
            <button type="submit" class="button primary">"Invite"</button>
        </form>
        {error_line(error)}
        {move || done.get().map(|(text, link)| view! {
            <p class="quiet" role="status">{text}</p>
            {link.map(|link| view! { <CopyLink link/> })}
        })}
    }
}

/// "Anyone with the link can join": makes a link that lasts 14 days, as often as needed.
#[component]
pub fn ShareLinkMaker(target: Target, changed: Callback<()>) -> impl IntoView {
    let role = RwSignal::new("viewer".to_owned());
    let error: Error = RwSignal::new(None);
    let made = RwSignal::new(None::<String>);
    let roles = target.has_roles();
    let create = move |_| {
        let target = target.clone();
        let role = roles.then(|| role.get_untracked());
        error.set(None);
        spawn_local(async move {
            let path = target.path(&["invitations"]);
            match api::invite(&target.api, &path, "", role.as_deref()).await {
                Ok(created) => {
                    made.try_set(link_to(&target, &created));
                    changed.run(());
                }
                Err(failure) => {
                    error.try_set(Some(failure.to_string()));
                }
            }
        });
    };
    view! {
        <div class="share-link">
            <span class="grow">
                {if roles { "Anyone with the link can join as" } else { "Anyone with the link can join this group." }}
            </span>
            {roles.then(|| view! { <RolePicker role label="Role the link gives"/> })}
            <button type="button" class="button" on:click=create>"Create link"</button>
        </div>
        {error_line(error)}
        {move || made.get().map(|link| view! { <CopyLink link/> })}
    }
}

/// The invitations nobody used up yet, each with Revoke.
#[component]
pub fn Pending(
    target: Target,
    #[prop(into)] invitations: Signal<Vec<Invitation>>,
    changed: Callback<()>,
) -> impl IntoView {
    let error: Error = RwSignal::new(None);
    let today = today();
    let row = move |invitation: Invitation| {
        let target = target.clone();
        let revoke = move |_| {
            if !confirm("Revoke this invitation? Its link stops working at once.") {
                return;
            }
            let target = target.clone();
            let id = invitation.id.to_string();
            act(error, changed, async move {
                api::remove(&target.api, &target.path(&["invitations", &id])).await
            });
        };
        view! {
            <li class="pending">
                <span class="grow">{invitation_text(&invitation, today)}</span>
                <button type="button" class="link-button danger" on:click=revoke>"Revoke"</button>
            </li>
        }
    };
    view! {
        <Show when=move || invitations.with(|listed| !listed.is_empty())
            fallback=|| view! { <p class="quiet">"No pending invitation."</p> }>
            <ul class="plain-list">
                <For each=move || invitations.get() key=|invitation| invitation.id children=row.clone()/>
            </ul>
        </Show>
        {error_line(error)}
    }
}

pub fn today() -> Date {
    let seconds = (js_sys::Date::now() / 1000.0) as i64;
    OffsetDateTime::from_unix_timestamp(seconds)
        .map(OffsetDateTime::date)
        .unwrap_or(Date::MIN)
}
