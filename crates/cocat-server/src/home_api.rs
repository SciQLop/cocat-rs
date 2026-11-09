//! The home page under `/api/me/home`: what waits for the caller, and what others changed in
//! their projects since they last opened them.

use std::cmp::Reverse;

use axum::Json;
use axum::extract::State;
use serde::Serialize;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use cocat_core::role::Role;

use crate::api::{ApiError, CurrentUser};
use crate::db::{self, RoomRow};
use crate::history::versions::Version;
use crate::history_api::{VersionView, room_versions, views};
use crate::invitations_api::PreviewOut;
use crate::openapi::Detail;
use crate::submissions_api::SubmissionOut;
use crate::{AppState, invitations, room, submissions};

/// Nothing older counts as new, whether or not the project was opened since.
const NEWS_WINDOW_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// The versions a project's card lists; the rest are counted in `more`.
const CHANGES_SHOWN: usize = 5;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(home))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct Home {
    /// Usable invitations addressed to the caller's verified email, newest first.
    invitations: Vec<AddressedInvitation>,
    /// For curators, the submissions waiting for review, oldest first; empty otherwise.
    reviews: Vec<SubmissionOut>,
    /// The caller's submissions still pending, then those accepted or declined in the last
    /// 30 days, newest first.
    submissions: Vec<SubmissionOut>,
    /// The caller's projects, most recently changed first; never changed ones last.
    projects: Vec<ProjectNews>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct AddressedInvitation {
    /// What `/api/invitations/{id}/accept` and `/decline` take.
    id: i64,
    #[serde(flatten)]
    invitation: PreviewOut,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct ProjectNews {
    id: String,
    name: String,
    role: &'static str,
    /// When the newest version ended, whoever made it.
    last_change_ms: Option<i64>,
    /// When the caller last opened the project; never when absent.
    seen_ms: Option<i64>,
    /// Others' versions that ended since the caller last opened the project (or within the
    /// last 30 days, whichever is later), newest first, at most 5, in the history's shape.
    changes: Vec<VersionView>,
    /// How many more such versions there are beyond `changes`.
    more: usize,
}

#[utoipa::path(
    get,
    path = "/api/me/home",
    security(("cookie" = [])),
    responses(
        (status = 200, description = "What waits for the caller and what changed in their projects", body = Home),
        (status = 401, description = "No valid session", body = Detail),
    )
)]
async fn home(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Home>, ApiError> {
    let curator = db::is_curator(&state.pool, user.id).await?;
    Ok(Json(Home {
        invitations: addressed(&state, &user).await?,
        reviews: reviews(&state, curator).await?,
        submissions: own_submissions(&state, user.id, curator).await?,
        projects: projects(&state, user.id).await?,
    }))
}

async fn addressed(
    state: &AppState,
    user: &db::User,
) -> Result<Vec<AddressedInvitation>, ApiError> {
    Ok(invitations::addressed_to(state, user)
        .await?
        .into_iter()
        .map(|(id, preview)| AddressedInvitation {
            id,
            invitation: preview.into(),
        })
        .collect())
}

async fn reviews(state: &AppState, curator: bool) -> Result<Vec<SubmissionOut>, ApiError> {
    if !curator {
        return Ok(Vec::new());
    }
    Ok(submissions::waiting(&state.pool)
        .await?
        .into_iter()
        .map(|submission| SubmissionOut::seen_by(submission, curator))
        .collect())
}

async fn own_submissions(
    state: &AppState,
    user: Uuid,
    curator: bool,
) -> Result<Vec<SubmissionOut>, ApiError> {
    Ok(submissions::recent(&state.pool, user)
        .await?
        .into_iter()
        .map(|submission| SubmissionOut::seen_by(submission, curator))
        .collect())
}

async fn projects(state: &AppState, user: Uuid) -> Result<Vec<ProjectNews>, ApiError> {
    let seen = db::rooms_seen(&state.pool, user).await?;
    let horizon = room::now_ms() - NEWS_WINDOW_MS;
    let mut projects = Vec::new();
    for (room, role) in db::list_user_rooms(&state.pool, user).await? {
        let seen_ms = seen.get(&room.id).copied();
        projects.push(project_news(state, user, room, role, seen_ms, horizon).await?);
    }
    projects.sort_by_key(|project| Reverse(project.last_change_ms));
    Ok(projects)
}

/// The project's card, with others' versions that ended after the caller last opened it,
/// and never before `horizon`.
async fn project_news(
    state: &AppState,
    user: Uuid,
    room: RoomRow,
    role: Role,
    seen_ms: Option<i64>,
    horizon: i64,
) -> Result<ProjectNews, ApiError> {
    let since = seen_ms.map_or(horizon, |seen| seen.max(horizon));
    let versions = room_versions(state, &room.id).await?;
    let last_change_ms = versions.iter().filter_map(|version| version.ended_ms).max();
    let news: Vec<Version> = versions
        .into_iter()
        .filter(|version| is_news(version, user, since))
        .collect();
    let more = news.len().saturating_sub(CHANGES_SHOWN);
    let shown = news.into_iter().take(CHANGES_SHOWN).collect();
    Ok(ProjectNews {
        changes: views(state, &room.id, role, shown).await?,
        id: room.id,
        name: room.name,
        role: role.as_str(),
        last_change_ms,
        seen_ms,
        more,
    })
}

/// Someone else's version that ended after `since`; one's own changes are not news.
fn is_news(version: &Version, user: Uuid, since: i64) -> bool {
    version.author != Some(user) && version.ended_ms.is_some_and(|ended| ended > since)
}
