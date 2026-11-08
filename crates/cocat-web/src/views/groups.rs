//! The visitor's groups, and one group: its members, the projects it shares and, for its
//! owner, its invitations.

use leptos::html::Input;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;

use crate::api::{self, ApiError};
use crate::history::browse_href;
use crate::projects::Project;
use crate::route::Route;
use crate::sharing::{Group, Groups, Invitation, Target, group_member_removable};
use crate::views::controls::{Error, Hint, confirm, error_line, go_to};
use crate::views::invite::{InviteByEmail, Pending, ShareLinkMaker, act};
use crate::views::projects::Projects;

#[component]
pub fn GroupsPage(shown: Memo<Route>) -> impl IntoView {
    view! {
        <main class="content groups-page">
            {move || match shown.get() {
                Route::Group(id) => view! { <GroupView id/> }.into_any(),
                _ => view! { <GroupList/> }.into_any(),
            }}
        </main>
    }
}

fn page() -> String {
    window().location().pathname().unwrap_or_default()
}

fn api() -> String {
    expect_context::<Projects>().api()
}

#[component]
fn GroupList() -> impl IntoView {
    let projects = expect_context::<Projects>();
    let groups = RwSignal::new(None::<Vec<Group>>);
    let error: Error = RwSignal::new(None);
    let reload = Callback::new(move |()| {
        let api = projects.api();
        spawn_local(async move {
            match api::read::<Groups>(&api, &["groups"]).await {
                Ok(listed) => {
                    groups.try_set(Some(listed.groups));
                }
                Err(failure) => {
                    error.try_set(Some(failure.to_string()));
                }
            }
        });
    });
    reload.run(());
    let row = move |group: Group| view! { <GroupRow group reload/> };
    view! {
        <div class="groups-head">
            <h1>"Groups"</h1>
            <a class="button" href=Route::Default.href(&page())>"Back to catalogues"</a>
        </div>
        <Hint text="A group shares many projects with the same people at once." anchor="groups" topic="groups"/>
        <CreateGroup/>
        {error_line(error)}
        {move || groups.get().map(|listed| match listed.is_empty() {
            true => view! {
                <p class="quiet">"You are not in any group yet. Create one, or open an invitation link someone sent you."</p>
            }.into_any(),
            false => view! {
                <ul class="plain-list group-list">
                    <For each=move || listed.clone() key=|group| (group.id.clone(), group.name.clone()) children=row/>
                </ul>
            }.into_any(),
        })}
    }
}

#[component]
fn CreateGroup() -> impl IntoView {
    let projects = expect_context::<Projects>();
    let input = NodeRef::<Input>::new();
    let error: Error = RwSignal::new(None);
    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let name = input
            .get_untracked()
            .map(|input| input.value())
            .unwrap_or_default();
        let api = projects.api();
        spawn_local(async move {
            match api::create::<Group>(&api, &["groups"], json!({ "name": name })).await {
                Ok(group) => go_to(&Route::Group(group.id).hash()),
                Err(failure) => {
                    error.try_set(Some(failure.to_string()));
                }
            }
        });
    };
    view! {
        <form class="invite-row" on:submit=submit>
            <input type="text" required maxlength="200" node_ref=input placeholder="New group name"
                aria-label="New group name" on:input=move |_| error.set(None)/>
            <button type="submit" class="button primary">"Create group"</button>
        </form>
        {error_line(error)}
    }
}

#[component]
fn GroupRow(group: Group, reload: Callback<()>) -> impl IntoView {
    let href = Route::Group(group.id.clone()).href(&page());
    view! {
        <li class="group-row">
            <a class="grow" href=href>{group.name.clone()}</a>
            <span class="role">{group.role.clone()}</span>
            <GroupActions group changed=reload/>
        </li>
    }
}

/// Rename and delete for the owner, leave for a member.
#[component]
fn GroupActions(group: Group, changed: Callback<()>) -> impl IntoView {
    let projects = expect_context::<Projects>();
    let error: Error = RwSignal::new(None);
    let renaming = RwSignal::new(false);
    let id = StoredValue::new(group.id.clone());
    let name = StoredValue::new(group.name.clone());
    let delete = move |_| {
        if !confirm(&format!(
            "Delete {}? Its projects stop being shared with its members.",
            name.get_value()
        )) {
            return;
        }
        let (api, id) = (projects.api(), id.get_value());
        act(error, gone(changed), async move {
            api::remove(&api, &["groups", &id]).await
        });
    };
    let leave = move |_| {
        if !confirm(&format!("Leave {}?", name.get_value())) {
            return;
        }
        let (api, id) = (projects.api(), id.get_value());
        act(error, gone(changed), async move {
            api::leave_group(&api, &id).await
        });
    };
    let owner = group.role == "owner";
    view! {
        <span class="row-buttons">
            {owner.then(|| view! {
                <button type="button" class="link-button" on:click=move |_| renaming.set(true)>"Rename"</button>
                <button type="button" class="link-button danger" on:click=delete>"Delete"</button>
            })}
            {(!owner).then(|| view! {
                <button type="button" class="link-button" on:click=leave>"Leave"</button>
            })}
        </span>
        <Show when=move || renaming.get()>
            <RenameGroup id=id.get_value() name=name.get_value() open=renaming changed/>
        </Show>
        {error_line(error)}
    }
}

/// After a delete or a leave, the list is where the visitor lands.
fn gone(changed: Callback<()>) -> Callback<()> {
    Callback::new(move |()| {
        go_to(&Route::Groups.hash());
        changed.run(());
    })
}

#[component]
fn RenameGroup(
    id: String,
    name: String,
    open: RwSignal<bool>,
    changed: Callback<()>,
) -> impl IntoView {
    let projects = expect_context::<Projects>();
    let input = NodeRef::<Input>::new();
    let error: Error = RwSignal::new(None);
    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let name = input
            .get_untracked()
            .map(|input| input.value())
            .unwrap_or_default();
        let (api, id) = (projects.api(), id.clone());
        let done = Callback::new(move |()| {
            open.set(false);
            changed.run(());
        });
        act(error, done, async move {
            api::patch(&api, &["groups", &id], json!({ "name": name })).await
        });
    };
    view! {
        <form class="name-form group-rename" on:submit=submit>
            <label>
                <span class="field-label">"Group name"</span>
                <input type="text" required maxlength="200" node_ref=input value=name autofocus/>
            </label>
            {error_line(error)}
            <div class="row-buttons">
                <button type="submit" class="button primary">"Rename"</button>
                <button type="button" class="button" on:click=move |_| open.set(false)>"Cancel"</button>
            </div>
        </form>
    }
}

#[derive(Debug, Clone, Default)]
struct Detail {
    group: Option<Group>,
    members: Vec<Member>,
    projects: Vec<Project>,
    invitations: Vec<Invitation>,
    suggestions: Vec<String>,
}

/// A group member as `GET /api/groups/{id}/members` lists them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
struct Member {
    user_id: String,
    email: String,
    role: String,
}

#[derive(serde::Deserialize)]
struct Members {
    members: Vec<Member>,
}

async fn detail(target: Target) -> Result<Detail, ApiError> {
    let api = target.api.as_str();
    let mine: Groups = api::read(api, &["groups"]).await?;
    let Some(group) = mine.groups.into_iter().find(|group| group.id == target.id) else {
        return Ok(Detail::default());
    };
    let members: Members = api::read(api, &target.path(&["members"])).await?;
    let projects = api::read(api, &target.path(&["rooms"])).await?;
    let (invitations, suggestions) = match group.role.as_str() {
        "owner" => (
            api::read(api, &target.path(&["invitations"])).await?,
            api::read(api, &target.path(&["suggestions"])).await?,
        ),
        _ => (Vec::new(), Vec::new()),
    };
    Ok(Detail {
        group: Some(group),
        members: members.members,
        projects,
        invitations,
        suggestions,
    })
}

#[component]
fn GroupView(id: String) -> impl IntoView {
    let target = Target::group(&api(), &id);
    let loaded = RwSignal::new(None::<Detail>);
    let error: Error = RwSignal::new(None);
    let changed = {
        let target = target.clone();
        Callback::new(move |()| {
            let target = target.clone();
            spawn_local(async move {
                match detail(target).await {
                    Ok(found) => {
                        loaded.try_set(Some(found));
                    }
                    Err(failure) => {
                        error.try_set(Some(failure.to_string()));
                    }
                }
            });
        })
    };
    changed.run(());
    let back = Route::Groups.href(&page());
    // Only the group itself rebuilds the view: a reload after an invitation keeps the link
    // just made on screen.
    let group =
        Memo::new(move |_| loaded.with(|found| found.as_ref().map(|found| found.group.clone())));
    view! {
        <p class="crumbs"><a href=back.clone()>"Groups"</a>" /"</p>
        {error_line(error)}
        {move || group.get().map(|group| match group {
            None => view! {
                <p class="quiet">"This group does not exist, or you are not in it."</p>
            }.into_any(),
            Some(group) => view! { <GroupBody target=target.clone() group loaded changed/> }.into_any(),
        })}
    }
}

fn part<T: Clone + Send + Sync + 'static>(
    loaded: RwSignal<Option<Detail>>,
    pick: fn(&Detail) -> &Vec<T>,
) -> Signal<Vec<T>> {
    Signal::derive(move || {
        loaded.with(|found| {
            found
                .as_ref()
                .map(|found| pick(found).clone())
                .unwrap_or_default()
        })
    })
}

#[component]
fn GroupBody(
    target: Target,
    group: Group,
    loaded: RwSignal<Option<Detail>>,
    changed: Callback<()>,
) -> impl IntoView {
    let owner = group.role == "owner";
    let members = part(loaded, |found| &found.members);
    let row = {
        let role = group.role.clone();
        let target = target.clone();
        move |member: Member| view! { <GroupMember target=target.clone() visitor=role.clone() member changed/> }
    };
    view! {
        <div class="groups-head">
            <h1>{group.name.clone()}</h1>
            <span class="role">{format!("Your role: {}", group.role)}</span>
            <GroupActions group=group.clone() changed/>
        </div>
        <section class="share-section">
            <h2 class="eyebrow">"Members"</h2>
            <ul class="plain-list people">
                <For each=move || members.get() key=|member| member.user_id.clone() children=row/>
            </ul>
        </section>
        <section class="share-section">
            <h2 class="eyebrow">"Projects shared with this group"</h2>
            <Hint text="Every member gets these projects, with the role shown, including people who join later." anchor="groups" topic="groups"/>
            <SharedProjects listed=part(loaded, |found| &found.projects)/>
        </section>
        <section class="share-section">
            <h2 class="eyebrow">"Invitations"</h2>
            {match owner {
                true => view! {
                    <InviteByEmail target=target.clone() suggestions=part(loaded, |found| &found.suggestions) changed/>
                    <ShareLinkMaker target=target.clone() changed/>
                    <h3 class="eyebrow">"Pending invitations"</h3>
                    <Pending target=target.clone() invitations=part(loaded, |found| &found.invitations) changed/>
                }.into_any(),
                false => view! { <p class="quiet">"Only the group's owner can invite people."</p> }.into_any(),
            }}
        </section>
    }
}

#[component]
fn GroupMember(
    target: Target,
    visitor: String,
    member: Member,
    changed: Callback<()>,
) -> impl IntoView {
    let error: Error = RwSignal::new(None);
    let removable = group_member_removable(&visitor, &member.role);
    let email = member.email.clone();
    let remove = move |_| {
        if !confirm(&format!("Remove {email} from the group?")) {
            return;
        }
        let (target, id) = (target.clone(), member.user_id.clone());
        act(error, changed, async move {
            api::remove(&target.api, &target.path(&["members", &id])).await
        });
    };
    view! {
        <li class="person">
            <span class="who"><span class="email">{member.email.clone()}</span></span>
            <span class="role">{member.role.clone()}</span>
            {removable.then(|| view! {
                <button type="button" class="link-button danger" on:click=remove.clone()>"Remove"</button>
            })}
        </li>
        {error_line(error)}
    }
}

#[component]
fn SharedProjects(listed: Signal<Vec<Project>>) -> impl IntoView {
    view! {
        <Show when=move || listed.with(|listed| !listed.is_empty())
            fallback=|| view! {
                <p class="quiet">"No project is shared with this group yet. A project's owner adds it from Share."</p>
            }>
            <ul class="plain-list">
                <For each=move || listed.get() key=|project| project.id.clone() let(project)>
                    <li class="person">
                        <a class="grow" href=browse_href(&project.id)>{project.name}</a>
                        <span class="role">{project.role.as_str()}</span>
                    </li>
                </For>
            </ul>
        </Show>
    }
}
