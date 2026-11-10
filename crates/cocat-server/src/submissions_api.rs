//! Submissions to the public catalogues under `/api/public`: anyone signed in offers one of
//! their catalogues, or a change to a public one made on a copy of it; curators accept or
//! decline; authors withdraw. Only the author and the curators see a submission. A plain
//! copy of a public catalogue, to use it rather than change it, goes through here too.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use cocat_core::model;
use cocat_core::role::Role;
use cocat_core::room_id::public_id;
use serde::{Deserialize, Serialize};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;
use yrs::{Doc, Transact};

use crate::AppState;
use crate::api::{ApiError, ApiPath, CurrentUser, JsonBody, membership};
use crate::copy_chain::{Source, keep_only_last_author, record_copy};
use crate::db::{self, PublicRoom};
use crate::history::diff::{Diff, before_and_after, diff, replay_until};
use crate::history::versions::{apply, newest};
use crate::history_api::{copy_name, encoded, make_copy, save_newest, saved};
use crate::lifecycle;
use crate::openapi::Detail;
use crate::public_api::{description, folder_or_source};
use crate::room::Author;
use crate::submissions::{
    self, ACCEPTED, DECLINED, Decision, NewSubmission, Submission, WITHDRAWN,
};

const NEW: &str = "new";
const A_CURATOR: &str = "a curator";
const UPDATE: &str = "update";
const NO_SUCH_SUBMISSION: &str = "No such submission";
const NO_SUCH_CATALOGUE: &str = "No such catalogue in this project";
const NO_SUCH_PUBLIC: &str = "No such public catalogue";
const NOT_A_COPY: &str = "This catalogue is not a copy of a public catalogue";
const ONLY_CURATORS: &str = "Only curators review submissions";
const ONLY_THE_AUTHOR: &str = "Only its author can withdraw a submission";
const DECIDED: &str = "This submission was already decided or withdrawn";
const TARGET_GONE: &str = "The public catalogue this submission changes was deleted";
const EMPTY_PUBLIC: &str = "This public catalogue holds no catalogue yet";
const INTO_PUBLIC: &str =
    "Copy it into one of your projects: a public catalogue changes through submissions";
const HELD_ALREADY: &str = "This project already holds a copy of this catalogue";
const BAD_TITLE: &str = "Enter a title of at most 200 characters";
const LONG_NOTE: &str = "Enter a note of at most 2000 characters";
const MAX_TITLE: usize = 200;
const MAX_NOTE: usize = 2000;
/// A version name's limit.
const MAX_VERSION_NAME: usize = 100;
/// A slug this long leaves room for "-<n>" within a key's 64 characters.
const MAX_SLUG: usize = 56;
/// Snapshots stay in the database, so one submission may not take a project's room.
const MAX_SNAPSHOT: u64 = 50 * 1_048_576;
/// How many submissions one author may have waiting for a curator.
const MAX_PENDING: i64 = 20;
const TOO_MANY_PENDING: &str = "You have 20 submissions waiting for a curator: withdraw one, \
    or wait for a decision, before submitting another";

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, submit))
        .routes(routes!(submit_update))
        .routes(routes!(one))
        .routes(routes!(review))
        .routes(routes!(withdraw))
        .routes(routes!(propose))
        .routes(routes!(copy))
        .routes(routes!(proposals))
}

/// A submission as its author and the curators see it.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct SubmissionOut {
    id: i64,
    /// `new`, or `update` for a change to a public catalogue.
    kind: String,
    /// The author's email.
    author: String,
    /// The public catalogue an update changes, or the one an accepted new one became.
    target: Option<String>,
    title: String,
    folder: Option<String>,
    description: Option<String>,
    /// The author's note to the curators.
    note: Option<String>,
    /// `pending`, `accepted`, `declined` or `withdrawn`.
    status: String,
    /// The curator who decided it: their email for curators, "a curator" for its author.
    reviewer: Option<String>,
    /// The curator's note.
    review_note: Option<String>,
    /// When it was submitted (RFC 3339).
    created: String,
    /// When it was accepted, declined or withdrawn.
    decided: Option<String>,
}

impl SubmissionOut {
    /// Only curators see who reviewed it; its author sees "a curator".
    pub(crate) fn seen_by(submission: Submission, curator: bool) -> Self {
        let reviewer = if curator {
            submission.reviewer_email.clone()
        } else {
            submission
                .reviewer_email
                .as_ref()
                .map(|_| A_CURATOR.to_owned())
        };
        Self {
            reviewer,
            ..submission.into()
        }
    }
}

impl From<Submission> for SubmissionOut {
    fn from(submission: Submission) -> Self {
        Self {
            id: submission.id,
            kind: submission.kind,
            author: submission.author_email,
            target: submission.target,
            title: submission.title,
            folder: submission.folder,
            description: submission.description,
            note: submission.note,
            status: submission.status,
            reviewer: submission.reviewer_email,
            review_note: submission.review_note,
            created: submission.created_at,
            decided: submission.decided_at,
        }
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct SubmissionDetail {
    #[serde(flatten)]
    submission: SubmissionOut,
    /// The public catalogue changed since the author copied it.
    stale: bool,
    /// What accepting it changes: against the public catalogue as it is now for an update,
    /// every event for a new one.
    changes: Diff,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct NewBody {
    /// The project holding the catalogue.
    room: String,
    catalogue: String,
    /// The catalogue's name when left out.
    title: Option<String>,
    folder: Option<String>,
    description: Option<String>,
    note: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct UpdateBody {
    /// The project holding the copy.
    room: String,
    catalogue: String,
    note: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct ReviewBody {
    accept: bool,
    note: Option<String>,
}

#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
struct ProposeBody {
    /// One of the caller's projects; a new project when left out.
    room: Option<String>,
    /// The new project's name.
    name: Option<String>,
}

#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
struct CopyBody {
    /// One of the caller's projects; a new project, named after the public catalogue, when
    /// left out.
    room: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ListQuery {
    #[serde(default)]
    mine: bool,
}

/// Where a copy of a public catalogue went.
#[derive(Debug, Serialize, utoipa::ToSchema)]
struct Copied {
    /// The project holding the copy.
    room: String,
    /// That project's name.
    name: String,
    /// The copied catalogue's uuid, the public one's.
    catalogue: String,
}

/// A catalogue copied out of a public catalogue.
#[derive(Debug, Serialize, utoipa::ToSchema)]
struct ProposalOut {
    catalogue: String,
    /// The public catalogue's id.
    target: String,
    /// The public catalogue's name.
    name: String,
}

#[utoipa::path(
    get,
    path = "/api/public/submissions",
    params(("mine" = Option<bool>, Query, description = "Only the caller's own; curators otherwise get everyone's")),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The submissions, pending first, then the newest", body = [SubmissionOut]),
        (status = 401, description = "No valid session", body = Detail),
    )
)]
async fn list(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<SubmissionOut>>, ApiError> {
    let curator = db::is_curator(&state.pool, user.id).await?;
    let author = (query.mine || !curator).then_some(user.id);
    let listed = submissions::list(&state.pool, author).await?;
    let seen = listed
        .into_iter()
        .map(|submission| SubmissionOut::seen_by(submission, curator))
        .collect();
    Ok(Json(seen))
}

#[utoipa::path(
    post,
    path = "/api/public/submissions",
    request_body = NewBody,
    security(("cookie" = [])),
    responses(
        (status = 201, description = "The catalogue and its events as they are now, waiting for a curator", body = SubmissionOut),
        (status = 400, description = "A field is blank or too long", body = Detail),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller cannot write to the project", body = Detail),
        (status = 404, description = "No such project, membership or catalogue", body = Detail),
        (status = 409, description = "The caller has 20 submissions waiting already", body = Detail),
        (status = 413, description = "The catalogue is over 50 MB or over what the caller's storage has left", body = Detail),
        (status = 422, description = "The body is not a submission", body = Detail),
    )
)]
async fn submit(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    JsonBody(body): JsonBody<NewBody>,
) -> Result<(StatusCode, Json<SubmissionOut>), ApiError> {
    lifecycle::require_role(&state, &body.room, user.id, Role::Editor).await?;
    let (name, snapshot) = snapshot_of(
        &state,
        &body.room,
        &body.catalogue,
        user.id,
        Some(&user.email),
    )
    .await?;
    let title = title(body.title.unwrap_or(name))?;
    let folder = folder_or_source(body.folder)?;
    let description = description(body.description)?;
    let note = note(body.note)?;
    let new = NewSubmission {
        author: user.id,
        room: &body.room,
        kind: NEW,
        target: None,
        base: None,
        catalogue: &body.catalogue,
        title: &title,
        folder: folder.as_deref(),
        description: description.as_deref(),
        note: note.as_deref(),
        snapshot: &snapshot,
    };
    let id = submissions::insert(&state.pool, &new).await?;
    Ok((StatusCode::CREATED, Json(stored(&state, id, false).await?)))
}

#[utoipa::path(
    post,
    path = "/api/public/submissions/updates",
    request_body = UpdateBody,
    security(("cookie" = [])),
    responses(
        (status = 201, description = "The copy as it is now, offered as the public catalogue's next version", body = SubmissionOut),
        (status = 400, description = "The note is too long", body = Detail),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller cannot write to the project", body = Detail),
        (status = 404, description = "No such project, membership or catalogue, or it is no copy of a public one", body = Detail),
        (status = 409, description = "The caller has 20 submissions waiting already, or the public catalogue follows a provider", body = Detail),
        (status = 413, description = "The catalogue is over 50 MB or over what the caller's storage has left", body = Detail),
        (status = 422, description = "The body is not a submission", body = Detail),
    )
)]
async fn submit_update(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    JsonBody(body): JsonBody<UpdateBody>,
) -> Result<(StatusCode, Json<SubmissionOut>), ApiError> {
    lifecycle::require_role(&state, &body.room, user.id, Role::Editor).await?;
    let proposal = submissions::proposals(&state.pool, &body.room)
        .await?
        .into_iter()
        .find(|proposal| proposal.catalogue == body.catalogue)
        .ok_or_else(|| ApiError::NotFound(NOT_A_COPY.to_owned()))?;
    lifecycle::refuse_relayed(&state, &proposal.target).await?;
    let note = note(body.note)?;
    let (_, snapshot) = snapshot_of(&state, &body.room, &body.catalogue, user.id, None).await?;
    let new = NewSubmission {
        author: user.id,
        room: &body.room,
        kind: UPDATE,
        target: Some(&proposal.target),
        base: Some(proposal.base),
        catalogue: &body.catalogue,
        title: &proposal.name,
        folder: None,
        description: None,
        note: note.as_deref(),
        snapshot: &snapshot,
    };
    let id = submissions::insert(&state.pool, &new).await?;
    Ok((StatusCode::CREATED, Json(stored(&state, id, false).await?)))
}

#[utoipa::path(
    get,
    path = "/api/public/submissions/{id}",
    params(("id" = i64, Path, description = "The submission's id")),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The submission and what accepting it changes", body = SubmissionDetail),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such submission, or the caller is neither its author nor a curator", body = Detail),
    )
)]
async fn one(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> Result<Json<SubmissionDetail>, ApiError> {
    let (submission, curator) = visible(&state, id, user.id).await?;
    let snapshot = submissions::snapshot(&state.pool, id).await?;
    let (changes, stale) = what_changed(&state, &submission, snapshot).await?;
    Ok(Json(SubmissionDetail {
        submission: SubmissionOut::seen_by(submission, curator),
        stale,
        changes,
    }))
}

#[utoipa::path(
    post,
    path = "/api/public/submissions/{id}/review",
    params(("id" = i64, Path, description = "The submission's id")),
    request_body = ReviewBody,
    security(("cookie" = [])),
    responses(
        (status = 200, description = "Accepted (the public catalogue now holds it) or declined", body = SubmissionOut),
        (status = 400, description = "The note is too long", body = Detail),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller is its author, not a curator", body = Detail),
        (status = 404, description = "No such submission, or the caller is neither its author nor a curator", body = Detail),
        (status = 409, description = "It was decided or withdrawn already, its public catalogue was deleted or follows a provider, or it does not fit", body = Detail),
        (status = 422, description = "The body is not a review", body = Detail),
    )
)]
async fn review(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(id): ApiPath<i64>,
    JsonBody(body): JsonBody<ReviewBody>,
) -> Result<Json<SubmissionOut>, ApiError> {
    let (submission, curator) = visible(&state, id, user.id).await?;
    if !curator {
        return Err(ApiError::Forbidden(ONLY_CURATORS.to_owned()));
    }
    if let (true, Some(target)) = (body.accept, &submission.target) {
        lifecycle::refuse_relayed(&state, target).await?;
    }
    let note = note(body.note)?;
    let decision = Decision {
        status: if body.accept { ACCEPTED } else { DECLINED },
        by: user.id,
        note: note.as_deref(),
    };
    if !submissions::decide(&state.pool, id, &decision).await? {
        return Err(ApiError::Conflict(DECIDED.to_owned()));
    }
    if body.accept
        && let Err(error) = take_in(&state, &submission, user.id, note.as_deref()).await
    {
        submissions::reopen(&state.pool, id).await?;
        return Err(error);
    }
    Ok(Json(stored(&state, id, true).await?))
}

#[utoipa::path(
    post,
    path = "/api/public/submissions/{id}/withdraw",
    params(("id" = i64, Path, description = "The submission's id")),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "Withdrawn: curators no longer review it", body = SubmissionOut),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller is a curator, not its author", body = Detail),
        (status = 404, description = "No such submission, or the caller is neither its author nor a curator", body = Detail),
        (status = 409, description = "It was decided or withdrawn already", body = Detail),
    )
)]
async fn withdraw(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> Result<Json<SubmissionOut>, ApiError> {
    let (submission, curator) = visible(&state, id, user.id).await?;
    if !submission.is_by(user.id) {
        return Err(ApiError::Forbidden(ONLY_THE_AUTHOR.to_owned()));
    }
    let decision = Decision {
        status: WITHDRAWN,
        by: user.id,
        note: None,
    };
    if !submissions::decide(&state.pool, id, &decision).await? {
        return Err(ApiError::Conflict(DECIDED.to_owned()));
    }
    Ok(Json(stored(&state, id, curator).await?))
}

#[utoipa::path(
    post,
    path = "/api/public/{room_id}/proposals",
    params(("room_id" = String, Path, description = "The public catalogue's id")),
    request_body = ProposeBody,
    security(("cookie" = [])),
    responses(
        (status = 201, description = "A copy of the public catalogue, same uuids, in the project given or a new one; it remembers where it came from", body = Copied),
        (status = 400, description = "The new project's name has control characters", body = Detail),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller cannot write to the project given", body = Detail),
        (status = 404, description = "No such public catalogue, project or membership", body = Detail),
        (status = 409, description = "The project is public, holds a copy already, or is full; or the public catalogue is empty or follows a provider", body = Detail),
        (status = 422, description = "The body is not a proposal", body = Detail),
    )
)]
async fn propose(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
    JsonBody(body): JsonBody<ProposeBody>,
) -> Result<(StatusCode, Json<Copied>), ApiError> {
    let public = public_room(&state, &room_id).await?;
    lifecycle::refuse_relayed(&state, &room_id).await?;
    let name = body.name.unwrap_or_else(|| proposal_name(&public.name));
    let (copied, base) = copy_public(&state, &user, &public, body.room, name).await?;
    submissions::link(
        &state.pool,
        &copied.room,
        &copied.catalogue,
        &public.id,
        base,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(copied)))
}

#[utoipa::path(
    post,
    path = "/api/public/{room_id}/copies",
    params(("room_id" = String, Path, description = "The public catalogue's id")),
    request_body = CopyBody,
    security(("cookie" = [])),
    responses(
        (status = 201, description = "A copy of the public catalogue, same uuids and name, in the project given or a new one; it does not link back", body = Copied),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller cannot write to the project given", body = Detail),
        (status = 404, description = "No such public catalogue, project or membership", body = Detail),
        (status = 409, description = "The project is public, holds a copy already, or is full; or the public catalogue is empty", body = Detail),
        (status = 422, description = "The body is not a copy", body = Detail),
    )
)]
async fn copy(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
    JsonBody(body): JsonBody<CopyBody>,
) -> Result<(StatusCode, Json<Copied>), ApiError> {
    let public = public_room(&state, &room_id).await?;
    let name = public.name.chars().take(lifecycle::MAX_NAME).collect();
    let (copied, _) = copy_public(&state, &user, &public, body.room, name).await?;
    Ok((StatusCode::CREATED, Json(copied)))
}

#[utoipa::path(
    get,
    path = "/api/rooms/{room_id}/proposals",
    params(("room_id" = String, Path, description = "Project id (a room in the API)")),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The project's catalogues copied out of public catalogues", body = [ProposalOut]),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such project, or the caller is not a member", body = Detail),
    )
)]
async fn proposals(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
) -> Result<Json<Vec<ProposalOut>>, ApiError> {
    membership(&state, &room_id, user.id).await?;
    let listed = submissions::proposals(&state.pool, &room_id).await?;
    let out = listed
        .into_iter()
        .map(|proposal| ProposalOut {
            catalogue: proposal.catalogue,
            target: proposal.target,
            name: proposal.name,
        })
        .collect();
    Ok(Json(out))
}

/// The submission, once the caller may see it: its author or a curator. Anyone else is
/// told there is none. Says whether the caller is a curator.
async fn visible(state: &AppState, id: i64, user: Uuid) -> Result<(Submission, bool), ApiError> {
    let submission = submissions::one(&state.pool, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(NO_SUCH_SUBMISSION.to_owned()))?;
    let curator = db::is_curator(&state.pool, user).await?;
    if !curator && !submission.is_by(user) {
        return Err(ApiError::NotFound(NO_SUCH_SUBMISSION.to_owned()));
    }
    Ok((submission, curator))
}

async fn stored(state: &AppState, id: i64, curator: bool) -> Result<SubmissionOut, ApiError> {
    let submission = submissions::one(&state.pool, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(NO_SUCH_SUBMISSION.to_owned()))?;
    Ok(SubmissionOut::seen_by(submission, curator))
}

/// The catalogue's name and the catalogue alone as one update, once `author` may store it.
/// A new submission passes the email of `copier`, and its snapshot then records the copy out
/// of the project as it is now; an update does not, as accepting it keeps the target's chain.
async fn snapshot_of(
    state: &AppState,
    room_id: &str,
    catalogue: &str,
    author: Uuid,
    copier: Option<&str>,
) -> Result<(String, Vec<u8>), ApiError> {
    if submissions::pending(&state.pool, author).await? >= MAX_PENDING {
        return Err(ApiError::Conflict(TOO_MANY_PENDING.to_owned()));
    }
    let uuid = catalogue.to_owned();
    let (name, snapshot) = state
        .rooms
        .read(room_id, move |doc| extracted(doc, &uuid))
        .await?
        .ok_or_else(|| ApiError::NotFound(NO_SUCH_CATALOGUE.to_owned()))?;
    let snapshot = match copier {
        Some(email) => recorded_copy(state, room_id, email, snapshot).await?,
        None => snapshot,
    };
    let size = u64::try_from(snapshot.len()).unwrap_or(u64::MAX);
    fits(size, state.rooms.limits().room)?;
    let left = state.rooms.owner_left(&state.pool, author, None).await?;
    within_quota(size, left, "Your", "submission")?;
    Ok((name, snapshot))
}

/// A catalogue outside the trash, alone, and its name.
fn extracted(doc: &Doc, catalogue: &str) -> Option<(String, Vec<u8>)> {
    let (name, alone) = extract_alone(doc, catalogue)?;
    Some((name, encoded(&alone)))
}

fn extract_alone(doc: &Doc, catalogue: &str) -> Option<(String, Doc)> {
    let found = model::catalogue(&doc.transact(), catalogue).filter(|found| !found.deleted)?;
    Some((found.name, model::extract(doc, catalogue)))
}

fn fits(size: u64, room_limit: u64) -> Result<(), ApiError> {
    let limit = room_limit.min(MAX_SNAPSHOT);
    if size <= limit {
        return Ok(());
    }
    Err(ApiError::TooLarge(format!(
        "This catalogue is too big to submit: it takes {}, and a submission holds at most {}",
        bytes(size),
        bytes(limit)
    )))
}

/// Refuses `size` bytes of `what` when the `whose` storage has only `left` bytes left.
pub(crate) fn within_quota(size: u64, left: u64, whose: &str, what: &str) -> Result<(), ApiError> {
    if size <= left {
        return Ok(());
    }
    Err(ApiError::TooLarge(format!(
        "{whose} storage is full: this {what} takes {}, and {} is left",
        bytes(size),
        bytes(left)
    )))
}

pub(crate) fn bytes(size: u64) -> String {
    const KB: u64 = 1024;
    match size {
        0..KB => format!("{size} bytes"),
        KB..=1_048_575 => format!("{} kB", size.div_ceil(KB)),
        _ => format!("{:.1} MB", size as f64 / 1_048_576.0),
    }
}

/// The public catalogue's catalogue, as one update holding it alone and recording `by`
/// copying it, and how many changes the public catalogue had: the copy's base.
async fn public_copy(
    state: &AppState,
    public: &PublicRoom,
    by: &str,
) -> Result<(String, Vec<u8>, i64), ApiError> {
    let saved = saved(state, &public.id).await?;
    let (id, name, by) = (public.id.clone(), public.name.clone(), by.to_owned());
    state
        .rooms
        .read_log(&public.id, 1, move |records, slots| {
            let doc = replay_until(records, u64::MAX);
            let uuid = model::catalogues(&doc.transact()).into_iter().next()?.uuid;
            let (_, alone) = extract_alone(&doc, &uuid)?;
            let source = Source {
                project: &id,
                project_name: &name,
                version: newest(slots, &saved),
                by: &by,
            };
            record_copy(&alone, &source);
            Some((uuid, encoded(&alone), count(records.len())))
        })
        .await?
        .ok_or_else(|| ApiError::Conflict(EMPTY_PUBLIC.to_owned()))
}

async fn public_room(state: &AppState, room_id: &str) -> Result<PublicRoom, ApiError> {
    db::public_room(&state.pool, room_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(NO_SUCH_PUBLIC.to_owned()))
}

/// Copies the public catalogue into `room`, or a new project named `name`; where it went
/// and the copy's base.
async fn copy_public(
    state: &AppState,
    user: &db::User,
    public: &PublicRoom,
    room: Option<String>,
    name: String,
) -> Result<(Copied, i64), ApiError> {
    let (catalogue, update, base) = public_copy(state, public, &user.email).await?;
    let (room, name) = match room {
        Some(room) => copy_into(state, &room, user.id, &catalogue, update).await?,
        None => {
            let first = copy_name(&public.name, None);
            let making = make_copy(state.clone(), user.id, name, update, first);
            let made = tokio::spawn(making).await.map_err(|error| {
                tracing::warn!(%error, "a copy stopped half way");
                ApiError::Internal
            })??;
            (made.id, made.name)
        }
    };
    let copied = Copied {
        room,
        name,
        catalogue,
    };
    Ok((copied, base))
}

/// Writes the copy into one of the caller's projects; its id and name.
async fn copy_into(
    state: &AppState,
    room_id: &str,
    user: Uuid,
    catalogue: &str,
    update: Vec<u8>,
) -> Result<(String, String), ApiError> {
    lifecycle::require_role(state, room_id, user, Role::Editor).await?;
    if db::is_public(&state.pool, room_id).await? {
        return Err(ApiError::Conflict(INTO_PUBLIC.to_owned()));
    }
    let uuid = catalogue.to_owned();
    let held = state
        .rooms
        .read(room_id, move |doc| {
            model::catalogue(&doc.transact(), &uuid).is_some()
        })
        .await?;
    if held {
        return Err(ApiError::Conflict(HELD_ALREADY.to_owned()));
    }
    let allowance = state.rooms.allowance(&state.pool, room_id).await?;
    state
        .rooms
        .write_as(room_id, allowance, Author::Client(user), update)
        .await?;
    let name = db::room_name(&state.pool, room_id)
        .await?
        .unwrap_or_default();
    Ok((room_id.to_owned(), name))
}

/// What accepting changes, worked out by accepting it on a replay of the public catalogue,
/// and whether the public catalogue changed since the author copied it.
async fn what_changed(
    state: &AppState,
    submission: &Submission,
    snapshot: Vec<u8>,
) -> Result<(Diff, bool), ApiError> {
    let catalogue = submission.catalogue.clone();
    let target = submission
        .target
        .as_deref()
        .filter(|_| submission.kind == UPDATE);
    let Some(target) = target else {
        return Ok((
            diff(&Doc::new(), &doc_of(&snapshot), Some(&catalogue)),
            false,
        ));
    };
    let (changes, records) = state
        .rooms
        .read_log(target, 3, move |records, _| {
            let (current, accepted) = before_and_after(records, records.len(), records.len());
            model::adopt(&mut accepted.transact_mut(), &doc_of(&snapshot), &catalogue);
            let changes = diff(&current, &accepted, Some(&catalogue));
            (changes, count(records.len()))
        })
        .await?;
    let moved = submission.base.is_some_and(|base| records > base);
    Ok((changes, submission.is_pending() && moved))
}

/// Writes an accepted submission into its public catalogue, a new one for a new
/// submission, as one change by its author, then saves that version under the
/// submission's name.
async fn take_in(
    state: &AppState,
    submission: &Submission,
    curator: Uuid,
    note: Option<&str>,
) -> Result<(), ApiError> {
    let snapshot = submissions::snapshot(&state.pool, submission.id).await?;
    let author = Uuid::parse_str(&submission.author).map_err(|_| ApiError::Internal)?;
    let (target, written) = match (submission.kind.as_str(), &submission.target) {
        (UPDATE, Some(target)) => {
            let written = adopt_into(state, target, submission, snapshot, author).await?;
            let base = state
                .rooms
                .read_log(target, 0, |records, _| count(records.len()))
                .await?;
            submissions::rebase(&state.pool, submission.id, base).await?;
            (target.clone(), written)
        }
        (UPDATE, None) => return Err(ApiError::Conflict(TARGET_GONE.to_owned())),
        _ => (
            new_public(state, submission, curator, snapshot, author).await?,
            true,
        ),
    };
    if written {
        let name: String = format!("Submission {}: {}", submission.id, submission.title)
            .chars()
            .take(MAX_VERSION_NAME)
            .collect();
        save_newest(state, &target, curator, Some(&name), note).await?;
    }
    Ok(())
}

/// Makes the public catalogue's catalogue the snapshot; false when it already was.
async fn adopt_into(
    state: &AppState,
    target: &str,
    submission: &Submission,
    snapshot: Vec<u8>,
    author: Uuid,
) -> Result<bool, ApiError> {
    let catalogue = submission.catalogue.clone();
    let update = state
        .rooms
        .read_log(target, 2, move |records, _| {
            let present = replay_until(records, u64::MAX);
            let mut txn = present.transact_mut();
            model::adopt(&mut txn, &doc_of(&snapshot), &catalogue);
            txn.encode_update_v1()
        })
        .await?;
    let allowance = state.rooms.allowance(&state.pool, target).await?;
    Ok(state
        .rooms
        .write_as(target, allowance, Author::Client(author), update)
        .await?)
}

/// A public catalogue named after the submission, holding its snapshot; its id. Removed
/// again when the snapshot cannot be written.
async fn new_public(
    state: &AppState,
    submission: &Submission,
    curator: Uuid,
    snapshot: Vec<u8>,
    author: Uuid,
) -> Result<String, ApiError> {
    let snapshot = without_copiers(snapshot);
    let room = create_public(state, submission, curator).await?;
    let allowance = state.rooms.allowance(&state.pool, &room.id).await?;
    let written = state
        .rooms
        .write_as(&room.id, allowance, Author::Client(author), snapshot)
        .await;
    if let Err(error) = written {
        if let Err(cleanup) = lifecycle::delete_room(state, &room.id, curator).await {
            tracing::warn!(error = %cleanup, room = %room.id, "cannot remove a failed public catalogue");
        }
        return Err(error.into());
    }
    submissions::set_target(&state.pool, submission.id, &room.id).await?;
    Ok(room.id)
}

/// The snapshot recording that `by` copied it out of project `room_id` at its newest version.
async fn recorded_copy(
    state: &AppState,
    room_id: &str,
    by: &str,
    snapshot: Vec<u8>,
) -> Result<Vec<u8>, ApiError> {
    let project_name = db::room_name(&state.pool, room_id)
        .await?
        .unwrap_or_default();
    let saved = saved(state, room_id).await?;
    let version = state
        .rooms
        .read_log(room_id, 0, move |_, slots| newest(slots, &saved))
        .await?;
    let doc = doc_of(&snapshot);
    let source = Source {
        project: room_id,
        project_name: &project_name,
        version,
        by,
    };
    record_copy(&doc, &source);
    Ok(encoded(&doc))
}

/// The snapshot as a public catalogue keeps it: only its submitting author is named.
fn without_copiers(snapshot: Vec<u8>) -> Vec<u8> {
    let doc = doc_of(&snapshot);
    keep_only_last_author(&doc);
    encoded(&doc)
}

/// The public catalogue `public.<slug of the title>`, with `-2`, `-3`… when taken. Taken is
/// what the insert meets, so two curators accepting one title at once get one id each.
async fn create_public(
    state: &AppState,
    submission: &Submission,
    curator: Uuid,
) -> Result<PublicRoom, ApiError> {
    let key = key(&submission.title);
    let mut tried = 1;
    loop {
        let candidate = match tried {
            1 => key.clone(),
            n => format!("{key}-{n}"),
        };
        let room = PublicRoom {
            id: public_id(&candidate).map_err(|_| ApiError::Internal)?,
            name: submission.title.clone(),
            folder: submission.folder.clone(),
            source: None,
            description: submission.description.clone(),
            public_count: None,
            counted_file: None,
        };
        if db::create_public_room(&state.pool, &room, Some(curator)).await? {
            return Ok(room);
        }
        tried += 1;
    }
}

/// "<public name> (proposed change)", the public name cut so the whole fits a project name.
fn proposal_name(public: &str) -> String {
    const SUFFIX: &str = " (proposed change)";
    let kept: String = public
        .chars()
        .take(lifecycle::MAX_NAME - SUFFIX.len())
        .collect();
    format!("{}{SUFFIX}", kept.trim_end())
}

/// The title as a key: lower-case ASCII letters, digits and dashes.
fn key(title: &str) -> String {
    let slug: String = slug::slugify(title).chars().take(MAX_SLUG).collect();
    match slug.trim_matches('-') {
        "" => "catalogue".to_owned(),
        slug => slug.to_owned(),
    }
}

fn doc_of(update: &[u8]) -> Doc {
    let doc = Doc::new();
    apply(&doc, update);
    doc
}

fn count(records: usize) -> i64 {
    i64::try_from(records).unwrap_or(i64::MAX)
}

fn title(title: String) -> Result<String, ApiError> {
    let title = title.trim();
    match title.chars().count() {
        1..=MAX_TITLE if !title.chars().any(char::is_control) => Ok(title.to_owned()),
        _ => Err(ApiError::BadRequest(BAD_TITLE.to_owned())),
    }
}

fn note(note: Option<String>) -> Result<Option<String>, ApiError> {
    let note = note
        .map(|note| note.trim().to_owned())
        .filter(|note| !note.is_empty());
    match note {
        Some(note) if note.chars().count() > MAX_NOTE => {
            Err(ApiError::BadRequest(LONG_NOTE.to_owned()))
        }
        note => Ok(note),
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_SNAPSHOT, bytes, fits, key};
    use crate::api::ApiError;

    #[test]
    fn a_snapshot_over_fifty_megabytes_is_refused_whatever_the_project_limit() {
        assert!(fits(MAX_SNAPSHOT, u64::MAX).is_ok());
        let Err(ApiError::TooLarge(detail)) = fits(50 * 1_048_576 + 1, u64::MAX) else {
            panic!("a snapshot over 50 MB was taken");
        };
        assert_eq!(
            detail,
            "This catalogue is too big to submit: it takes 50.0 MB, and a submission holds at most 50.0 MB"
        );
    }

    #[test]
    fn a_key_is_the_title_in_plain_lower_case() {
        assert_eq!(key("Ondes de choc é"), "ondes-de-choc-e");
        assert_eq!(key("ACE: shocks (2020)!"), "ace-shocks-2020");
        assert_eq!(key("日本"), "ri-ben");
        assert_eq!(key("???"), "catalogue");
        assert!(key(&"long words ".repeat(20)).len() <= 56);
    }

    #[test]
    fn sizes_read_in_plain_units() {
        assert_eq!(bytes(200), "200 bytes");
        assert_eq!(bytes(1500), "2 kB");
        assert_eq!(bytes(3 * 1_048_576), "3.0 MB");
    }
}
