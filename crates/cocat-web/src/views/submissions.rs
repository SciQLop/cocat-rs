//! Submissions to the public catalogues: the forms that send one, the curators' queue, the
//! visitor's own, and one submission with what it changes.

use cocat_core::model;
use cocat_core::role::Role;
use cocat_core::room_id::is_public_id;
use leptos::html::{Input, Select, Textarea};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;

use crate::api;
use crate::history::{catalogue_line, diff_counts, diff_rows};
use crate::projects::Project;
use crate::public::PublicCatalogue;
use crate::route::Route;
use crate::state::Room;
use crate::submissions::{
    Detail, Proposal, Submission, SubmitFields, copy_body, copy_href, copy_target, folders,
    pending, review_body, submit_body, update_body,
};
use crate::views::actions::Form;
use crate::views::controls::{Error, Hint, confirm, error_line};
use crate::views::history::{DiffTable, legend};
use crate::views::projects::Projects;

/// Events shown at most in What changed.
// simplify: a plain table cut at this many rows, as the history does.
const MOST_ROWS: usize = 2_000;

/// Offers the catalogue on screen as a new public catalogue.
#[component]
pub fn SubmitForm(uuid: String, form: RwSignal<Form>) -> impl IntoView {
    let room = expect_context::<Room>();
    let name = room
        .read_untracked(|txn| model::catalogue(txn, &uuid))
        .map(|found| found.name)
        .unwrap_or_default();
    let api = room.config.api_url.clone();
    let suggestions = RwSignal::new(Vec::<String>::new());
    let reading = api.clone();
    spawn_local(async move {
        if let Ok(index) = api::read::<Vec<PublicCatalogue>>(&reading, &["public"]).await {
            suggestions.try_set(folders(&index));
        }
    });
    let (title, folder) = (NodeRef::<Input>::new(), NodeRef::<Input>::new());
    let (description, note) = (NodeRef::<Textarea>::new(), NodeRef::<Textarea>::new());
    let error: Error = RwSignal::new(None);
    let sent = RwSignal::new(false);
    let project = room.config.room_id.clone();
    let send = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let fields = SubmitFields {
            title: title.get().map(|input| input.value()).unwrap_or_default(),
            folder: folder.get().map(|input| input.value()).unwrap_or_default(),
            description: description
                .get()
                .map(|area| area.value())
                .unwrap_or_default(),
            note: note.get().map(|area| area.value()).unwrap_or_default(),
        };
        let body = submit_body(&project, &uuid, &fields);
        let api = api.clone();
        spawn_local(async move {
            let made = api::create::<Submission>(&api, &["public", "submissions"], body).await;
            match made {
                Ok(_) => sent.set(true),
                Err(failure) => error.set(Some(failure.to_string())),
            }
        });
    };
    view! {
        <Show when=move || !sent.get() fallback=move || view! { <Sent form/> }>
            <form class="inline-form name-form submit-form" on:submit=send.clone()>
                <Hint anchor="public" topic="submitting a catalogue"
                    text="The curators get this catalogue and its events as they are now. Your project does not change."/>
                <label>
                    <span class="field-label">"Title"</span>
                    <input type="text" maxlength="200" node_ref=title value=name.clone()/>
                </label>
                <label>
                    <span class="field-label">"Folder (optional)"</span>
                    <input type="text" maxlength="200" list="public-folders" node_ref=folder
                        placeholder="Such as AMDA/Solar wind"/>
                    <datalist id="public-folders">
                        {move || suggestions.get().into_iter().map(|folder| view! { <option value=folder/> }).collect_view()}
                    </datalist>
                </label>
                <label>
                    <span class="field-label">"Description (optional)"</span>
                    <textarea rows="3" maxlength="2000" node_ref=description></textarea>
                </label>
                <label>
                    <span class="field-label">"Note to the curators (optional)"</span>
                    <textarea rows="2" maxlength="2000" node_ref=note></textarea>
                </label>
                {error_line(error)}
                <div class="row-buttons">
                    <button type="submit" class="button primary">"Submit"</button>
                    <button type="button" class="button" on:click=move |_| form.set(Form::Closed)>"Cancel"</button>
                </div>
            </form>
        </Show>
    }
}

/// Offers the copy on screen as the next version of the public catalogue it came from.
#[component]
pub fn UpdateForm(uuid: String, proposal: Proposal, form: RwSignal<Form>) -> impl IntoView {
    let room = expect_context::<Room>();
    let api = room.config.api_url.clone();
    let project = room.config.room_id.clone();
    let note = NodeRef::<Textarea>::new();
    let error: Error = RwSignal::new(None);
    let sent = RwSignal::new(false);
    let send = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let typed = note.get().map(|area| area.value()).unwrap_or_default();
        let body = update_body(&project, &uuid, &typed);
        let api = api.clone();
        spawn_local(async move {
            let path = ["public", "submissions", "updates"];
            match api::create::<Submission>(&api, &path, body).await {
                Ok(_) => sent.set(true),
                Err(failure) => error.set(Some(failure.to_string())),
            }
        });
    };
    let target = proposal.name.clone();
    view! {
        <Show when=move || !sent.get() fallback=move || view! { <Sent form/> }>
            <form class="inline-form name-form submit-form" on:submit=send.clone()>
                <p class="quiet">
                    "The curators compare this catalogue with “" {target.clone()}
                    "” as it is now. If they accept, it becomes the public catalogue."
                </p>
                <label>
                    <span class="field-label">"What you changed, for the curators (optional)"</span>
                    <textarea rows="3" maxlength="2000" node_ref=note></textarea>
                </label>
                {error_line(error)}
                <div class="row-buttons">
                    <button type="submit" class="button primary">"Submit"</button>
                    <button type="button" class="button" on:click=move |_| form.set(Form::Closed)>"Cancel"</button>
                </div>
            </form>
        </Show>
    }
}

#[component]
fn Sent(form: RwSignal<Form>) -> impl IntoView {
    let page = page();
    view! {
        <div class="inline-form submit-form" role="status">
            <p>"Submitted. The curators will review it; you can follow it in My submissions."</p>
            <div class="row-buttons">
                <a class="button primary" href=Route::MySubmissions.href(&page)>"My submissions"</a>
                <button type="button" class="button" on:click=move |_| form.set(Form::Closed)>"Close"</button>
            </div>
        </div>
    }
}

/// Where a copy of a public catalogue went.
#[derive(Debug, Clone, Deserialize)]
struct Copied {
    room: String,
    name: String,
    catalogue: String,
}

/// Copies the public catalogue on screen into one of the visitor's projects, or a new one.
#[component]
pub fn ProposeForm(form: RwSignal<Form>) -> impl IntoView {
    let room = expect_context::<Room>();
    let public_name = room.config.room_name.clone();
    let made = RwSignal::new(None::<Copied>);
    let done = move || {
        made.get().map(|proposed| {
            let href = copy_href(&proposed.room, &proposed.catalogue);
            view! {
                <div class="inline-form submit-form" role="status">
                    <p>
                        "The copy is in “" {proposed.name} "”. Change it there, then choose "
                        <strong>"More › Submit as an update…"</strong>" to send it back to “" {public_name.clone()} "”."
                    </p>
                    <div class="row-buttons">
                        <a class="button primary" href=href>"Open the copy"</a>
                        <button type="button" class="button" on:click=move |_| form.set(Form::Closed)>"Close"</button>
                    </div>
                </div>
            }
        })
    };
    view! {
        <Show when=move || made.with(Option::is_none) fallback=done>
            <ProjectPicker public=room.config.room_id.clone() route="proposals" made action="Make the copy"
                close=Callback::new(move |()| form.set(Form::Closed))>
                <Hint anchor="public" topic="proposing a change"
                    text="This catalogue is copied into a project of yours. Change it there, then submit it as an update for the curators to review."/>
            </ProjectPicker>
        </Show>
    }
}

/// Copies public catalogue `public` into one of the visitor's projects, or a new one, to
/// use it there.
#[component]
pub fn CopyForm(public: String, close: Callback<()>) -> impl IntoView {
    let made = RwSignal::new(None::<Copied>);
    let done = move || {
        made.get().map(|copied| {
            let href = copy_href(&copied.room, &copied.catalogue);
            view! {
                <div class="inline-form submit-form" role="status">
                    <p>"Copied into “" {copied.name} "”."</p>
                    <div class="row-buttons">
                        <a class="button primary" href=href>"Open it"</a>
                        <button type="button" class="button" on:click=move |_| close.run(())>"Close"</button>
                    </div>
                </div>
            }
        })
    };
    view! {
        <Show when=move || made.with(Option::is_none) fallback=done>
            <ProjectPicker public=public.clone() route="copies" made action="Copy" close>
                <Hint anchor="public" topic="copying a public catalogue"
                    text="The catalogue and its events go into a project of yours, under the same name, to use and change as you like. The public catalogue does not change."/>
            </ProjectPicker>
        </Show>
    }
}

/// Picks one of the visitor's writable projects, or a new one, and asks the server to copy
/// public catalogue `public` there through `route` (`copies` or `proposals`).
#[component]
fn ProjectPicker(
    public: String,
    route: &'static str,
    made: RwSignal<Option<Copied>>,
    action: &'static str,
    close: Callback<()>,
    children: Children,
) -> impl IntoView {
    let projects = expect_context::<Projects>();
    let writable = move || {
        projects.list.with(|listed| {
            listed
                .iter()
                .filter(|project| project.role.can_write() && !is_public_id(&project.id))
                .cloned()
                .collect::<Vec<Project>>()
        })
    };
    let target = move || {
        let open = projects.open.with(|open| open.room_id.clone());
        projects.list.with(|listed| copy_target(listed, &open))
    };
    let into = NodeRef::<Select>::new();
    let error: Error = RwSignal::new(None);
    let send = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let picked = into.get().map(|select| select.value()).unwrap_or_default();
        let body = copy_body(Some(picked.as_str()).filter(|id| !id.is_empty()));
        let (api, public) = (projects.api(), public.clone());
        spawn_local(async move {
            let path = ["public", public.as_str(), route];
            match api::create::<Copied>(&api, &path, body).await {
                Ok(copied) => {
                    projects.reload();
                    made.set(Some(copied));
                }
                Err(failure) => error.set(Some(failure.to_string())),
            }
        });
    };
    view! {
        <form class="inline-form name-form submit-form" on:submit=send>
            {children()}
            <label>
                <span class="field-label">"Copy it into"</span>
                <select node_ref=into>
                    {move || {
                        let target = target();
                        let projects = writable().into_iter().map(|project| {
                            let picked = target.as_deref() == Some(project.id.as_str());
                            view! { <option value=project.id.clone() selected=picked>{project.name.clone()}</option> }
                        }).collect_view();
                        view! {
                            {projects}
                            <option value="" selected=target.is_none()>"A new project"</option>
                        }
                    }}
                </select>
            </label>
            {error_line(error)}
            <div class="row-buttons">
                <button type="submit" class="button primary">{action}</button>
                <button type="button" class="button" on:click=move |_| close.run(())>"Cancel"</button>
            </div>
        </form>
    }
}

/// How many submissions wait for review, read once; it stays `None` for a visitor who is
/// not a curator.
pub fn waiting_submissions(api: String) -> RwSignal<Option<usize>> {
    let waiting = RwSignal::new(None::<usize>);
    spawn_local(async move {
        if !api::is_curator(&api).await.unwrap_or(false) {
            return;
        }
        if let Ok(all) = api::read::<Vec<Submission>>(&api, &["public", "submissions"]).await {
            waiting.try_set(Some(pending(&all)));
        }
    });
    waiting
}

/// The curators' queue: the submissions waiting for review.
#[component]
pub fn SubmissionsPage() -> impl IntoView {
    let api = expect_context::<Projects>().api();
    let listed = RwSignal::new(None::<Result<Vec<Submission>, String>>);
    spawn_local(async move {
        let all = api::read::<Vec<Submission>>(&api, &["public", "submissions"]).await;
        let waiting = all.map(|all| all.into_iter().filter(Submission::is_pending).collect());
        listed.try_set(Some(waiting.map_err(|error| error.to_string())));
    });
    view! {
        <SubmissionList title="Submissions" listed mine=false empty="Nothing waits for review."
            hint="What people offer for the public catalogues. Open one to see what it changes, then accept or decline it."/>
    }
}

/// The visitor's own submissions, with their status and the curator's note.
#[component]
pub fn MySubmissionsPage() -> impl IntoView {
    let api = expect_context::<Projects>().api();
    let listed = RwSignal::new(None::<Result<Vec<Submission>, String>>);
    spawn_local(async move {
        let mine = api::my_submissions(&api).await;
        listed.try_set(Some(mine.map_err(|error| error.to_string())));
    });
    view! {
        <SubmissionList title="My submissions" listed mine=true empty="You have not submitted anything yet."
            hint="What you offered for the public catalogues, and what the curators said."/>
    }
}

#[component]
fn SubmissionList(
    title: &'static str,
    listed: RwSignal<Option<Result<Vec<Submission>, String>>>,
    /// The visitor's own: their address goes without saying.
    mine: bool,
    empty: &'static str,
    hint: &'static str,
) -> impl IntoView {
    let page = page();
    let back = Route::Public(None).href(&page);
    let rows = move || {
        listed.get().map(|listed| match listed {
            Err(error) => view! { <p class="field-error" role="alert">{error}</p> }.into_any(),
            Ok(listed) if listed.is_empty() => view! { <p class="quiet">{empty}</p> }.into_any(),
            Ok(listed) => {
                let page = page.clone();
                view! {
                    <ul class="plain-list submission-list">
                        {listed.into_iter().map(|submission| submission_row(submission, &page, mine)).collect_view()}
                    </ul>
                }
                .into_any()
            }
        })
    };
    view! {
        <main class="content groups-page public-page">
            <div class="groups-head">
                <h1>{title}</h1>
                <a class="button" href=back>"Back to public catalogues"</a>
            </div>
            <Hint text=hint anchor="public" topic="submissions"/>
            {rows}
        </main>
    }
}

fn submission_row(submission: Submission, page: &str, mine: bool) -> impl IntoView {
    let href = Route::Submission(submission.id).href(page);
    let status = status_class(&submission);
    let meta = match mine {
        true => format!("{} · {}", submission.kind_text(), submission.day()),
        false => format!(
            "{} · {} · {}",
            submission.kind_text(),
            submission.author,
            submission.day()
        ),
    };
    view! {
        <li>
            <a class="public-row submission-row" href=href>
                <span class="submission-top">
                    <span class="name">{submission.title.clone()}</span>
                    <span class=format!("chip {status}")>{submission.status_text()}</span>
                </span>
                <span class="meta">{meta}</span>
                {submission.note.clone().map(|note| view! { <span class="description">{note}</span> })}
                {submission.review_note.clone().map(|said| view! {
                    <span class="description review-said">{format!("Curator's note: {said}")}</span>
                })}
            </a>
        </li>
    }
}

/// One submission: who sent what, what accepting it changes, and the buttons its author or
/// a curator may use.
#[component]
pub fn SubmissionPage(id: i64) -> impl IntoView {
    let projects = expect_context::<Projects>();
    let api = projects.api();
    let me = projects
        .open
        .with_untracked(|config| config.user_email.clone());
    let detail = RwSignal::new(None::<Result<Detail, String>>);
    let curator = RwSignal::new(false);
    let load = {
        let api = api.clone();
        move || {
            let api = api.clone();
            spawn_local(async move {
                let id = id.to_string();
                let read = api::read::<Detail>(&api, &["public", "submissions", &id]).await;
                detail.try_set(Some(read.map_err(|error| error.to_string())));
            });
        }
    };
    load();
    let asking = api.clone();
    spawn_local(async move {
        curator.try_set(api::is_curator(&asking).await.unwrap_or(false));
    });
    let page = page();
    let back = Memo::new(move |_| match curator.get() {
        true => ("Back to submissions", Route::Submissions.href(&page)),
        false => ("Back to my submissions", Route::MySubmissions.href(&page)),
    });
    let body = move || {
        detail.get().map(|detail| match detail {
            Err(error) => view! { <p class="field-error" role="alert">{error}</p> }.into_any(),
            Ok(detail) => {
                let mine = detail.submission.author == me;
                let load = load.clone();
                view! { <SubmissionView detail curator mine reload=Callback::new(move |()| load())/> }
                    .into_any()
            }
        })
    };
    view! {
        <main class="content groups-page public-page submission-page">
            <div class="groups-head">
                <a class="button" href=move || back.get().1>{move || back.get().0}</a>
            </div>
            {body}
        </main>
    }
}

#[component]
fn SubmissionView(
    detail: Detail,
    curator: RwSignal<bool>,
    mine: bool,
    reload: Callback<()>,
) -> impl IntoView {
    let Detail {
        submission,
        stale,
        changes,
    } = detail;
    let (added, changed, removed) = diff_counts(&changes);
    let mut rows = diff_rows(&changes);
    let total = rows.len();
    rows.truncate(MOST_ROWS);
    let pending = submission.is_pending();
    let status = status_class(&submission);
    let meta = format!(
        "{} · from {} · submitted {}",
        submission.kind_text(),
        submission.author,
        submission.day()
    );
    let compared = match submission.kind.as_str() {
        "update" => "Compared with the public catalogue as it is now.",
        _ => "A new catalogue: every event is added.",
    };
    let id = submission.id;
    let open = submission.public().map(str::to_owned);
    let title = submission.title.clone();
    view! {
        <div class="groups-head">
            <h1>{submission.title.clone()}</h1>
            <span class=format!("chip {status}")>{submission.status_text()}</span>
        </div>
        <p class="quiet">{meta}</p>
        {submission.folder.clone().map(|folder| view! { <p class="quiet">{format!("Folder: {folder}")}</p> })}
        {submission.description.clone().map(|text| view! { <p class="submission-text">{text}</p> })}
        {submission.note.clone().map(|note| view! {
            <div class="submission-note"><span class="field-label">"Note to the curators"</span><p class="version-note">{note}</p></div>
        })}
        {submission.review_note.clone().map(|said| view! {
            <div class="submission-note">
                <span class="field-label">{submission.said_by()}</span>
                <p class="version-note">{said}</p>
            </div>
        })}
        {stale.then(|| view! {
            <p class="stale-banner" role="note">
                "The public catalogue changed since this was submitted. Accepting replaces those changes with this submission."
            </p>
        })}
        {open.map(|target| view! { <OpenPublic target title curator/> })}
        {(pending && mine).then(|| view! { <Withdraw id reload/> })}
        <Show when=move || pending && curator.get()>
            <Review id reload/>
        </Show>
        <h2 class="section-title">"What changed"</h2>
        <p class="legend">{legend(added, changed, removed)}</p>
        <ul class="catalogue-changes">
            {changes.catalogues.iter().map(|change| view! { <li>{catalogue_line(change)}</li> }).collect_view()}
        </ul>
        <DiffTable rows/>
        {(total > MOST_ROWS).then(|| view! {
            <p class="quiet">{format!("Showing the first {MOST_ROWS} of {total} events.")}</p>
        })}
        <p class="quiet">{compared}</p>
    }
}

#[component]
fn OpenPublic(target: String, title: String, curator: RwSignal<bool>) -> impl IntoView {
    let projects = expect_context::<Projects>();
    let open = move |_| {
        let role = if curator.get_untracked() {
            Role::Editor
        } else {
            Role::Viewer
        };
        projects.switch(&Project {
            id: target.clone(),
            name: title.clone(),
            role,
            editors_can_share: false,
            personal: false,
            files: false,
        });
    };
    view! {
        <div class="row-buttons">
            <button type="button" class="button" on:click=open>"Open the public catalogue"</button>
        </div>
    }
}

#[component]
fn Review(id: i64, reload: Callback<()>) -> impl IntoView {
    let api = expect_context::<Projects>().api();
    let note = NodeRef::<Textarea>::new();
    let error: Error = RwSignal::new(None);
    let busy = RwSignal::new(false);
    let decide = move |accept: bool| {
        let typed = note.get().map(|area| area.value()).unwrap_or_default();
        let api = api.clone();
        busy.set(true);
        spawn_local(async move {
            let id = id.to_string();
            let path = ["public", "submissions", id.as_str(), "review"];
            let decided = api::create::<Submission>(&api, &path, review_body(accept, &typed)).await;
            busy.try_set(false);
            match decided {
                Ok(_) => reload.run(()),
                Err(failure) => {
                    error.try_set(Some(failure.to_string()));
                }
            }
        });
    };
    let decline = decide.clone();
    view! {
        <div class="inline-form name-form review-form">
            <label>
                <span class="field-label">"Note to the author (optional)"</span>
                <textarea rows="2" maxlength="2000" node_ref=note></textarea>
            </label>
            {error_line(error)}
            <div class="row-buttons">
                <button type="button" class="button primary" disabled=move || busy.get()
                    on:click=move |_| decide(true)>"Accept"</button>
                <button type="button" class="button" disabled=move || busy.get()
                    on:click=move |_| decline(false)>"Decline"</button>
            </div>
        </div>
    }
}

#[component]
fn Withdraw(id: i64, reload: Callback<()>) -> impl IntoView {
    let api = expect_context::<Projects>().api();
    let error: Error = RwSignal::new(None);
    let withdraw = move |_| {
        if !confirm("Withdraw this submission? The curators will no longer review it.") {
            return;
        }
        let api = api.clone();
        spawn_local(async move {
            let id = id.to_string();
            let path = ["public", "submissions", id.as_str(), "withdraw"];
            let body = serde_json::json!({});
            match api::create::<Submission>(&api, &path, body).await {
                Ok(_) => reload.run(()),
                Err(failure) => {
                    error.try_set(Some(failure.to_string()));
                }
            }
        });
    };
    view! {
        <div class="row-buttons">
            <button type="button" class="button danger" on:click=withdraw>"Withdraw"</button>
        </div>
        {error_line(error)}
    }
}

fn status_class(submission: &Submission) -> String {
    format!("status-{}", submission.status)
}

fn page() -> String {
    window().location().pathname().unwrap_or_default()
}
