//! The home page: what waits on the visitor, then what others changed in their projects.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;

use crate::api;
use crate::history::{Version, browse_href, when_text};
use crate::home::{
    Home, Invitation, ProjectNews, ago, change_text, invitation_text, more_text, who,
};
use crate::route::Route;
use crate::submissions::Submission;
use crate::views::header::PublicHeader;
use crate::views::history::offset;
use crate::views::projects::{Projects, set_title};

type Loaded = RwSignal<Option<Result<Home, String>>>;

#[component]
pub fn HomeSpace() -> impl IntoView {
    let projects = expect_context::<Projects>();
    set_title("Home");
    let shown = Memo::new(move |_| projects.route.get());
    let loaded: Loaded = RwSignal::new(None);
    load(projects.api(), loaded);
    let body = move || {
        loaded.get().map(|answer| match answer {
            Err(error) => view! { <p class="field-error" role="alert">{error}</p> }.into_any(),
            Ok(home) => view! { <HomeBody home loaded/> }.into_any(),
        })
    };
    view! {
        <PublicHeader shown/>
        <div class="browser">
            <main class="content groups-page home-page">
                <h1>"Home"</h1>
                {body}
            </main>
        </div>
    }
}

fn load(api: String, loaded: Loaded) {
    spawn_local(async move {
        let home = api::read::<Home>(&api, &["me", "home"]).await;
        loaded.try_set(Some(home.map_err(|error| error.to_string())));
    });
}

#[component]
fn HomeBody(home: Home, loaded: Loaded) -> impl IntoView {
    let (news, quiet) = home.split();
    let waiting = home.waiting().then(|| {
        view! {
            <section class="home-section" aria-labelledby="waiting">
                <h2 id="waiting">"Waiting for you"</h2>
                <ul class="plain-list">
                    {home.invitations.into_iter().map(|invitation| view! { <InvitationRow invitation loaded/> }).collect_view()}
                    {home.reviews.into_iter().map(review_row).collect_view()}
                    {home.submissions.into_iter().map(submission_row).collect_view()}
                </ul>
            </section>
        }
    });
    let changed = match news.is_empty() {
        true => view! { <p class="quiet">"Nothing new from others since you last opened your projects."</p> }.into_any(),
        false => view! {
            <ul class="plain-list">{news.into_iter().map(news_card).collect_view()}</ul>
        }
        .into_any(),
    };
    let others = (!quiet.is_empty()).then(|| {
        view! {
            <section class="home-section" aria-labelledby="quiet">
                <h2 id="quiet">"Your other projects"</h2>
                <ul class="plain-list quiet-projects">{quiet.into_iter().map(quiet_row).collect_view()}</ul>
            </section>
        }
    });
    view! {
        {waiting}
        <section class="home-section" aria-labelledby="changed">
            <h2 id="changed">"What changed"</h2>
            {changed}
        </section>
        {others}
    }
}

/// An invitation to answer here: accepting opens the project.
#[component]
fn InvitationRow(invitation: Invitation, loaded: Loaded) -> impl IntoView {
    let projects = expect_context::<Projects>();
    let failed = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let id = invitation.id.to_string();
    let answer = move |accept: bool| {
        let api = projects.api();
        let id = id.clone();
        busy.set(true);
        spawn_local(async move {
            let path = [
                "invitations",
                id.as_str(),
                if accept { "accept" } else { "decline" },
            ];
            let done = match accept {
                true => api::create::<serde_json::Value>(&api, &path, json!({}))
                    .await
                    .map(|room| room["id"].as_str().map(browse_href)),
                false => api::post(&api, &path, json!({})).await.map(|()| None),
            };
            match done {
                Ok(Some(href)) => {
                    let _ = window().location().set_href(&href);
                }
                Ok(None) => load(api, loaded),
                Err(error) => {
                    busy.try_set(false);
                    failed.try_set(Some(error.to_string()));
                }
            }
        });
    };
    let decline = answer.clone();
    view! {
        <li class="home-card invitation">
            <div class="home-line">
                <span class="home-what">
                    <strong>{invitation.room_name.clone()}</strong>
                    <span class="quiet">{invitation_text(&invitation)}</span>
                </span>
                <span class="row-buttons">
                    <button type="button" class="button primary" disabled=busy on:click=move |_| answer(true)>"Accept"</button>
                    <button type="button" class="button" disabled=busy on:click=move |_| decline(false)>"Decline"</button>
                </span>
            </div>
            {move || failed.get().map(|error| view! { <p class="field-error" role="alert">{error}</p> })}
        </li>
    }
}

fn review_row(submission: Submission) -> impl IntoView {
    let href = Route::Submission(submission.id).href(&page());
    view! {
        <li>
            <a class="home-card home-link" href=href>
                <span class="home-line">
                    <span class="home-what">
                        <strong>{format!("Review “{}”", submission.title)}</strong>
                        <span class="quiet">{format!("{} · from {} · {}", submission.kind_text(), submission.author, submission.day())}</span>
                    </span>
                    <span class="chip status-pending">"To review"</span>
                </span>
            </a>
        </li>
    }
}

fn submission_row(submission: Submission) -> impl IntoView {
    let href = Route::Submission(submission.id).href(&page());
    let status = format!("chip status-{}", submission.status);
    view! {
        <li>
            <a class="home-card home-link" href=href>
                <span class="home-line">
                    <span class="home-what">
                        <strong>{submission.title.clone()}</strong>
                        <span class="quiet">{format!("Your submission · {}", submission.day())}</span>
                    </span>
                    <span class=status>{submission.status_text()}</span>
                </span>
                {submission.review_note.clone().map(|said| view! {
                    <span class="description review-said">{format!("Curator's note: {said}")}</span>
                })}
            </a>
        </li>
    }
}

fn news_card(project: ProjectNews) -> impl IntoView {
    let open = browse_href(&project.id);
    let history = format!("{open}{}", Route::Default.history().hash());
    let more = more_text(project.more);
    view! {
        <li class="home-card news">
            <div class="home-line">
                <a class="home-project" href=open>{project.name.clone()}</a>
                <span class="chip">{project.role.clone()}</span>
            </div>
            <ul class="changes">{project.changes.into_iter().map(change_row).collect_view()}</ul>
            <a class="more" href=history>{more.unwrap_or_else(|| "See the history".to_owned())}</a>
        </li>
    }
}

fn change_row(version: Version) -> impl IntoView {
    let color = version
        .author_color
        .clone()
        .unwrap_or_else(|| "var(--muted)".to_owned());
    let when = version
        .ended_ms
        .map(|ended| ago(ended, now_ms(), |ms| when_text(ms, offset(ms))));
    view! {
        <li>
            <span class="dot" style=format!("background: {color}") aria-hidden="true"></span>
            <strong>{who(&version)}</strong>
            <span>{change_text(&version)}</span>
            <span class="quiet when">{when}</span>
        </li>
    }
}

fn quiet_row(project: ProjectNews) -> impl IntoView {
    let last = match project.last_change_ms {
        Some(ms) => format!(
            "changed {}",
            ago(ms, now_ms(), |ms| when_text(ms, offset(ms)))
        ),
        None => "no changes yet".to_owned(),
    };
    view! {
        <li>
            <a class="home-link quiet-project" href=browse_href(&project.id)>
                <span class="home-project">{project.name.clone()}</span>
                <span class="quiet">{format!("{} · {last}", project.role)}</span>
            </a>
        </li>
    }
}

fn now_ms() -> i64 {
    js_sys::Date::now() as i64
}

fn page() -> String {
    window().location().pathname().unwrap_or_default()
}
