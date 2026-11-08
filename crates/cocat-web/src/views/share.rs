//! The Share dialog: who is in the project and, for whoever may share it, invitations, roles,
//! groups and share links. Each change is one request, after which the lists are read again.

use cocat_core::role::Role;
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;
use wasm_bindgen::JsValue;

use crate::api::{self, ApiError};
use crate::presence::initials;
use crate::route::opens_share;
use crate::sharing::{
    Group, Groups, Invitation, Member, Target, addable, colour_of, heirs, member_editable,
    sharing_note,
};
use crate::state::Room;
use crate::views::controls::{Error, Hint, confirm, error_line};
use crate::views::invite::{InviteByEmail, Pending, RolePicker, ShareLinkMaker, act};
use crate::views::projects::Projects;

#[derive(Debug, Clone, Default)]
struct Shared {
    members: Vec<Member>,
    groups: Vec<Group>,
    invitations: Vec<Invitation>,
    suggestions: Vec<String>,
    mine: Vec<Group>,
    /// False until read, so an editor sees no control the server would refuse.
    editors_can_share: bool,
}

async fn load(target: Target, visitor: Role) -> Result<Shared, ApiError> {
    let api = target.api.as_str();
    let members = api::read(api, &target.path(&["members"])).await?;
    let groups: Groups = api::read(api, &target.path(&["groups"])).await?;
    let editors_can_share = api::projects(api)
        .await?
        .iter()
        .any(|project| project.id == target.id && project.editors_can_share);
    if !visitor.may_share(editors_can_share) {
        return Ok(Shared {
            members,
            groups: groups.groups,
            editors_can_share,
            ..Shared::default()
        });
    }
    let mine: Groups = api::read(api, &["groups"]).await?;
    Ok(Shared {
        members,
        groups: groups.groups,
        invitations: api::read(api, &target.path(&["invitations"])).await?,
        suggestions: api::read(api, &target.path(&["suggestions"])).await?,
        mine: mine.groups,
        editors_can_share,
    })
}

/// The dialog's lists and the request that refreshes them.
#[derive(Clone, Copy)]
struct Share {
    target: StoredValue<Target>,
    visitor: Role,
    shared: RwSignal<Shared>,
    /// Whether the visitor may share, by the setting last read.
    sharer: Memo<bool>,
    error: Error,
}

impl Share {
    fn new(target: Target, visitor: Role) -> Self {
        let shared = RwSignal::new(Shared::default());
        Self {
            target: StoredValue::new(target),
            visitor,
            shared,
            sharer: Memo::new(move |_| {
                shared.with(|shared| visitor.may_share(shared.editors_can_share))
            }),
            error: RwSignal::new(None),
        }
    }

    fn reload(self) {
        let target = self.target.get_value();
        spawn_local(async move {
            match load(target, self.visitor).await {
                Ok(shared) => {
                    self.shared.try_set(shared);
                }
                Err(failure) => {
                    self.error.try_set(Some(failure.to_string()));
                }
            }
        });
    }

    fn changed(self) -> Callback<()> {
        Callback::new(move |()| self.reload())
    }

    fn run(self, request: impl Future<Output = Result<(), ApiError>> + 'static) {
        act(self.error, self.changed(), request);
    }

    fn list<T: Clone + Send + Sync + 'static>(
        self,
        pick: fn(&Shared) -> &Vec<T>,
    ) -> Signal<Vec<T>> {
        Signal::derive(move || self.shared.with(|shared| pick(shared).clone()))
    }
}

/// The header's Share button. The old project page sends people here with `#share`.
#[component]
pub fn ShareButton() -> impl IntoView {
    let open = RwSignal::new(take_share_hash());
    view! {
        <button type="button" class="button primary share-button" on:click=move |_| open.set(true)>"Share"</button>
        <Show when=move || open.get()>
            <ShareDialog open/>
        </Show>
    }
}

fn take_share_hash() -> bool {
    let location = window().location();
    if !opens_share(&location.hash().unwrap_or_default()) {
        return false;
    }
    let here = format!(
        "{}{}",
        location.pathname().unwrap_or_default(),
        location.search().unwrap_or_default()
    );
    if let Ok(history) = window().history() {
        let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&here));
    }
    true
}

#[component]
fn ShareDialog(open: RwSignal<bool>) -> impl IntoView {
    let projects = expect_context::<Projects>();
    let me = expect_context::<Room>().config.user_email.clone();
    let (target, visitor) = projects.open.with_untracked(|config| {
        (
            Target::project(&config.api_url, &config.room_id),
            config.role,
        )
    });
    let share = Share::new(target.clone(), visitor);
    share.reload();
    provide_context(share);
    let node = NodeRef::<html::Dialog>::new();
    Effect::new(move |_| {
        if let Some(node) = node.get() {
            let _ = node.show_modal();
        }
    });
    let name = move || projects.open.with(|config| config.room_name.clone());
    let transferring = RwSignal::new(false);
    let owner = visitor == Role::Owner;
    let note = move || {
        share
            .shared
            .with(|shared| sharing_note(visitor, shared.editors_can_share))
            .map(|note| view! { <p class="quiet">{note}</p> })
    };
    let body = move || {
        let target = target.clone();
        let me = me.clone();
        if transferring.get() {
            return view! { <Transfer me back=transferring/> }.into_any();
        }
        let sharer = share.sharer.get();
        view! {
            {sharer.then(|| view! {
                <InviteByEmail target=target.clone() suggestions=share.list(|shared| &shared.suggestions) changed=share.changed()/>
                <Hint text="Suggestions come from people you already work with. What each role can do:" anchor="roles" topic="roles"/>
            })}
            <People me/>
            <Grants/>
            {sharer.then(|| view! {
                <section class="share-section">
                    <h3 class="eyebrow">"Pending invitations"</h3>
                    <Pending target=target.clone() invitations=share.list(|shared| &shared.invitations) changed=share.changed()/>
                </section>
                <ShareLinkMaker target=target.clone() changed=share.changed()/>
            })}
            {note}
            {owner.then(|| view! { <EditorsShare/> })}
            {error_line(share.error)}
            <div class="share-foot">
                <Hint text="Everyone here sees every catalogue of this project." anchor="sharing" topic="sharing"/>
                {owner.then(|| view! {
                    <button type="button" class="link-button" on:click=move |_| transferring.set(true)>"Transfer ownership…"</button>
                })}
            </div>
        }
        .into_any()
    };
    view! {
        <dialog class="project-dialog share-dialog" node_ref=node aria-labelledby="share-title"
            on:close=move |_| open.set(false)>
            <div class="share-head">
                <h2 id="share-title">{move || format!("Share “{}”", name())}</h2>
                <button type="button" class="icon" aria-label="Close" on:click=move |_| open.set(false)>"✕"</button>
            </div>
            {body}
        </dialog>
    }
}

/// The members, each in their presence colour while they are here.
#[component]
fn People(me: String) -> impl IntoView {
    let share = expect_context::<Share>();
    let peers = expect_context::<Room>().peers;
    let row = move |member: Member| {
        let email = member.email.clone();
        let colour = {
            let email = email.clone();
            move || {
                peers.with(|peers| {
                    colour_of(&email, peers).map(|colour| format!("background: {colour}"))
                })
            }
        };
        let present = {
            let colour = colour.clone();
            move || colour().is_some()
        };
        let you = (member.email == me).then(|| view! { <span class="tag">"you"</span> });
        let controls = match member_editable(share.sharer.get_untracked(), member.role) {
            true => view! { <MemberControls member=member.clone()/> }.into_any(),
            false => view! { <span class="role">{member.role.as_str()}</span> }.into_any(),
        };
        view! {
            <li class="person">
                <span class="avatar" class:absent=move || !present() style=colour aria-hidden="true">{initials(&email)}</span>
                <span class="who"><span class="email">{email.clone()}</span>{you}</span>
                {controls}
            </li>
        }
    };
    view! {
        <section class="share-section">
            <h3 class="eyebrow">"People"</h3>
            <ul class="plain-list people">
                <For each=move || share.shared.with(|shared| shared.members.clone()) key=|member| (member.user_id.clone(), member.role) children=row/>
            </ul>
        </section>
    }
}

#[component]
fn MemberControls(member: Member) -> impl IntoView {
    let share = expect_context::<Share>();
    let role = RwSignal::new(member.role.as_str().to_owned());
    let id = StoredValue::new(member.user_id.clone());
    let change = Callback::new(move |picked: String| {
        let (target, id) = (share.target.get_value(), id.get_value());
        share.run(async move {
            api::patch(
                &target.api,
                &target.path(&["members", &id]),
                json!({ "role": picked }),
            )
            .await
        });
    });
    let email = member.email.clone();
    let remove = move |_| {
        if !confirm(&format!("Remove {email} from this project?")) {
            return;
        }
        let (target, id) = (share.target.get_value(), id.get_value());
        share.run(async move { api::remove(&target.api, &target.path(&["members", &id])).await });
    };
    view! {
        <RolePicker role label=format!("Role for {}", member.email) on_pick=change/>
        <button type="button" class="link-button danger" on:click=remove>"Remove"</button>
    }
}

/// The groups the project is shared with and, for whoever shares, adding one of theirs.
#[component]
fn Grants() -> impl IntoView {
    let share = expect_context::<Share>();
    let sharer = share.sharer.get_untracked();
    let row = move |group: Group| {
        let controls = match sharer {
            true => view! { <GrantControls group=group.clone()/> }.into_any(),
            false => view! { <span class="role">{group.role.clone()}</span> }.into_any(),
        };
        view! {
            <li class="person">
                <span class="avatar group" aria-hidden="true">{initials(&group.name)}</span>
                <span class="who"><span class="email">{group.name.clone()}</span><span class="tag">"group"</span></span>
                {controls}
            </li>
        }
    };
    let granted = share.list(|shared| &shared.groups);
    view! {
        <section class="share-section">
            <h3 class="eyebrow">"Groups"</h3>
            <Show when=move || granted.with(|granted| !granted.is_empty())
                fallback=|| view! { <p class="quiet">"No group has access to this project."</p> }>
                <ul class="plain-list people">
                    <For each=move || granted.get() key=|group| (group.id.clone(), group.role.clone()) children=row/>
                </ul>
            </Show>
            {sharer.then(|| view! { <AddGroup/> })}
        </section>
    }
}

#[component]
fn GrantControls(group: Group) -> impl IntoView {
    let share = expect_context::<Share>();
    let role = RwSignal::new(group.role.clone());
    let id = StoredValue::new(group.id.clone());
    let change = Callback::new(move |picked: String| {
        let (target, id) = (share.target.get_value(), id.get_value());
        share.run(async move {
            api::put(
                &target.api,
                &target.path(&["groups", &id]),
                json!({ "role": picked }),
            )
            .await
        });
    });
    let name = group.name.clone();
    let remove = move |_| {
        if !confirm(&format!("Stop sharing this project with {name}?")) {
            return;
        }
        let (target, id) = (share.target.get_value(), id.get_value());
        share.run(async move { api::remove(&target.api, &target.path(&["groups", &id])).await });
    };
    view! {
        <RolePicker role label=format!("Role for {}", group.name) on_pick=change/>
        <button type="button" class="link-button danger" on:click=remove>"Remove"</button>
    }
}

#[component]
fn AddGroup() -> impl IntoView {
    let share = expect_context::<Share>();
    let choices = Memo::new(move |_| {
        share
            .shared
            .with(|shared| addable(&shared.mine, &shared.groups))
    });
    let picked = RwSignal::new(String::new());
    let role = RwSignal::new("viewer".to_owned());
    let add = move |_| {
        let chosen = picked.get_untracked();
        let id = choices.with_untracked(|choices| {
            choices
                .iter()
                .find(|group| group.id == chosen)
                .or(choices.first())
                .map(|group| group.id.clone())
        });
        let Some(id) = id else {
            return;
        };
        let target = share.target.get_value();
        let role = role.get_untracked();
        share.run(async move {
            api::put(
                &target.api,
                &target.path(&["groups", &id]),
                json!({ "role": role }),
            )
            .await
        });
    };
    view! {
        <Show when=move || choices.with(|choices| !choices.is_empty())>
            <div class="invite-row">
                <select aria-label="Group to add" on:change=move |event| picked.set(event_target_value(&event))>
                    <For each=move || choices.get() key=|group| group.id.clone() let(group)>
                        <option value=group.id>{group.name}</option>
                    </For>
                </select>
                <RolePicker role label="Role for the group"/>
                <button type="button" class="button" on:click=add>"Add group"</button>
            </div>
        </Show>
    }
}

/// The owner's switch that lets editors share, or keeps sharing to the owner.
#[component]
fn EditorsShare() -> impl IntoView {
    let share = expect_context::<Share>();
    let checked = move || share.shared.with(|shared| shared.editors_can_share);
    let change = move |event: leptos::ev::Event| {
        let allowed = event_target_checked(&event);
        let target = share.target.get_value();
        share.run(async move {
            api::put(
                &target.api,
                &target.path(&["editors-can-share"]),
                json!({ "allowed": allowed }),
            )
            .await
        });
    };
    view! {
        <label class="switch share-switch">
            <input type="checkbox" prop:checked=checked on:change=change/>
            "Editors can change access and share"
        </label>
    }
}

/// Hands the project to another member, after saying what that means.
#[component]
fn Transfer(me: String, back: RwSignal<bool>) -> impl IntoView {
    let share = expect_context::<Share>();
    let heirs: Vec<Member> = share
        .shared
        .with_untracked(|shared| heirs(&shared.members, &me).into_iter().cloned().collect());
    let picked = RwSignal::new(heirs.first().map(|member| member.user_id.clone()));
    let error: Error = RwSignal::new(None);
    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let Some(user_id) = picked.get_untracked() else {
            return;
        };
        let target = share.target.get_value();
        let done = Callback::new(|()| {
            let location = window().location();
            let _ = location.set_hash("share");
            let _ = location.reload();
        });
        act(error, done, async move {
            api::post(
                &target.api,
                &target.path(&["owner"]),
                json!({ "user_id": user_id }),
            )
            .await
        });
    };
    if heirs.is_empty() {
        return view! {
            <p>"Nobody else is in this project yet. Invite someone first."</p>
            <div class="row-buttons">
                <button type="button" class="button" on:click=move |_| back.set(false)>"Back"</button>
            </div>
        }
        .into_any();
    }
    view! {
        <form class="name-form" on:submit=submit>
            <h3>"Transfer ownership"</h3>
            <label>
                <span class="field-label">"New owner"</span>
                <select on:change=move |event| picked.set(Some(event_target_value(&event)))>
                    {heirs.into_iter().map(|member| view! {
                        <option value=member.user_id>{member.email}</option>
                    }).collect_view()}
                </select>
            </label>
            <p>"They can then invite people, change roles and delete the project. You stay in it as an editor."</p>
            {error_line(error)}
            <div class="row-buttons">
                <button type="submit" class="button danger">"Transfer ownership"</button>
                <button type="button" class="button" on:click=move |_| back.set(false)>"Back"</button>
            </div>
        </form>
    }
    .into_any()
}
