//! The HTTP side of selection rounds: settings, members, state moves and what each caller sees.

use std::collections::{BTreeMap, HashMap};

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use cocat_core::model;
use cocat_core::role::Role;
use cocat_core::round::{budget_json, parse_budget};
use cocat_core::time::Timestamp;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;
use yrs::Transact;

use crate::api::{ApiError, ApiPath, CurrentUser, JsonBody, membership};
use crate::history::diff::replay_until;
use crate::openapi::Detail;
use crate::room::Author;
use crate::rounds::rules::{self, Seat};
use crate::rounds::{
    self, Kind, RoundPatch, RoundRole, RoundRow, RoundSpec, RoundState, Verdict, VerdictRow,
    Visibility,
};
use crate::{AppState, db, lifecycle, ws};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_rounds, create_round))
        .routes(routes!(get_round, patch_round))
        .routes(routes!(put_members))
        .routes(routes!(move_round))
        .routes(routes!(my_proposal))
        .routes(routes!(put_votes, get_votes))
        .routes(routes!(put_verdicts, get_verdicts))
        .routes(routes!(put_verdict))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct ProposalOut {
    author: String,
    /// Absent when the caller may not read this proposal yet.
    doc: Option<Uuid>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct LogOut {
    at: String,
    actor: Option<String>,
    what: String,
    note: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct RoundOut {
    id: Uuid,
    project: String,
    name: String,
    kind: Kind,
    state: RoundState,
    /// Starts at 1; each move back into proposing adds one.
    revision: i64,
    visibility: Visibility,
    deadline: Option<String>,
    /// The budget as compact JSON-LD.
    #[schema(value_type = Option<Object>)]
    budget: Option<Value>,
    final_catalogue: Option<String>,
    proposers: Vec<String>,
    voters: Vec<String>,
    deciders: Vec<String>,
    proposals: Vec<ProposalOut>,
    log: Vec<LogOut>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct NewRound {
    name: String,
    /// Defaults to `downlink` with a budget, else `labelling`.
    kind: Option<Kind>,
    visibility: Visibility,
    deadline: Option<String>,
    #[schema(value_type = Option<Object>)]
    budget: Option<Value>,
}

fn set_or_clear<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(deserializer).map(Some)
}

/// For `deadline` and `budget`, an absent key leaves the setting alone and `null` clears it.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct RoundChange {
    name: Option<String>,
    kind: Option<Kind>,
    visibility: Option<Visibility>,
    #[serde(default, deserialize_with = "set_or_clear")]
    #[schema(value_type = Option<String>)]
    deadline: Option<Option<String>>,
    #[serde(default, deserialize_with = "set_or_clear")]
    #[schema(value_type = Option<Object>)]
    budget: Option<Option<Value>>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct RoundMembers {
    proposers: Vec<String>,
    voters: Vec<String>,
    deciders: Vec<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct Move {
    state: RoundState,
    /// The catalogue of the project that is the outcome; required to decide.
    final_catalogue: Option<String>,
    /// Free text kept with the move in the round's log, at most 2000 characters.
    note: Option<String>,
}

const NOTE_LIMIT: usize = 2000;

fn checked_note(note: Option<&str>) -> Result<Option<&str>, ApiError> {
    match note {
        Some(note) if note.chars().count() > NOTE_LIMIT => Err(unprocessable(format!(
            "A note is at most {NOTE_LIMIT} characters"
        ))),
        note => Ok(note),
    }
}

fn unknown_round(round: &str) -> ApiError {
    ApiError::NotFound(format!("Round {round} not found"))
}

fn forbidden(detail: &str) -> ApiError {
    ApiError::Forbidden(detail.to_owned())
}

fn unprocessable(detail: impl Into<String>) -> ApiError {
    ApiError::Unprocessable(detail.into())
}

/// A round of the project and the caller's place in it; 404 for anyone not in the project.
async fn load(
    state: &AppState,
    room_id: &str,
    round: &str,
    user: Uuid,
) -> Result<(RoundRow, Seat), ApiError> {
    let project_role = membership(state, room_id, user).await?;
    let id = round.parse::<Uuid>().map_err(|_| unknown_round(round))?;
    let row = rounds::round(&state.pool, id)
        .await?
        .filter(|row| row.room_id == room_id)
        .ok_or_else(|| unknown_round(round))?;
    let roles = rounds::roles_of(&rounds::members(&state.pool, id).await?, user);
    Ok((
        row,
        Seat {
            project_role,
            roles,
        },
    ))
}

fn require_administrator(seat: &Seat) -> Result<(), ApiError> {
    match rules::may_administer(seat) {
        true => Ok(()),
        false => Err(forbidden("Only a decider or a project owner may do this")),
    }
}

fn refuse_decided(round: &RoundRow) -> Result<(), ApiError> {
    match round.state {
        RoundState::Decided => Err(ApiError::Conflict(
            "A decided round cannot be changed".to_owned(),
        )),
        _ => Ok(()),
    }
}

fn stored_budget(budget: &Value) -> Result<String, ApiError> {
    let parsed = parse_budget(budget).map_err(|error| unprocessable(error.to_string()))?;
    Ok(budget_json(&parsed).to_string())
}

fn checked_deadline(deadline: &str) -> Result<String, ApiError> {
    Timestamp::parse(deadline)
        .and_then(|instant| instant.rfc3339().ok())
        .ok_or_else(|| unprocessable("the deadline must be an ISO 8601 instant"))
}

fn clean_name(name: &str) -> Result<String, ApiError> {
    match name.trim() {
        "" => Err(unprocessable("Enter a round name")),
        name => Ok(name.to_owned()),
    }
}

struct Emails(HashMap<Uuid, String>);

impl Emails {
    async fn load(state: &AppState, ids: impl Iterator<Item = Uuid>) -> Result<Self, ApiError> {
        let mut found = HashMap::new();
        for id in ids {
            if let std::collections::hash_map::Entry::Vacant(slot) = found.entry(id) {
                let email = db::user_by_id(&state.pool, id).await?.map(|u| u.email);
                slot.insert(email.unwrap_or_else(|| id.to_string()));
            }
        }
        Ok(Self(found))
    }

    fn of(&self, id: Uuid) -> String {
        self.0.get(&id).cloned().unwrap_or_else(|| id.to_string())
    }

    fn with_role(&self, members: &[(Uuid, RoundRole)], role: RoundRole) -> Vec<String> {
        let mut emails: Vec<String> = members
            .iter()
            .filter(|(_, held)| *held == role)
            .map(|(id, _)| self.of(*id))
            .collect();
        emails.sort();
        emails
    }
}

async fn present(state: &AppState, round: RoundRow, viewer: Uuid) -> Result<RoundOut, ApiError> {
    let members = rounds::members(&state.pool, round.id).await?;
    let proposals = rounds::proposals(&state.pool, round.id).await?;
    let log = rounds::log(&state.pool, round.id).await?;
    let ids = members
        .iter()
        .map(|(id, _)| *id)
        .chain(proposals.iter().map(|(author, _)| *author))
        .chain(log.iter().filter_map(|line| line.actor));
    let emails = Emails::load(state, ids).await?;
    let seat = Seat {
        project_role: membership(state, &round.room_id, viewer).await?,
        roles: rounds::roles_of(&members, viewer),
    };
    Ok(RoundOut {
        proposers: emails.with_role(&members, RoundRole::Proposer),
        voters: emails.with_role(&members, RoundRole::Voter),
        deciders: emails.with_role(&members, RoundRole::Decider),
        proposals: proposals
            .into_iter()
            .map(|(author, doc)| ProposalOut {
                author: emails.of(author),
                doc: rules::proposal_role(&round, &seat, author == viewer).map(|_| doc),
            })
            .collect(),
        log: log
            .into_iter()
            .map(|line| LogOut {
                at: line.at,
                actor: line.actor.map(|actor| emails.of(actor)),
                what: line.what,
                note: line.note,
            })
            .collect(),
        budget: round
            .budget
            .as_deref()
            .and_then(|b| serde_json::from_str(b).ok()),
        id: round.id,
        project: round.room_id,
        name: round.name,
        kind: round.kind,
        state: round.state,
        revision: round.revision,
        visibility: round.visibility,
        deadline: round.deadline,
        final_catalogue: round.final_catalogue,
    })
}

#[utoipa::path(
    get,
    path = "/api/rooms/{room_id}/rounds",
    params(("room_id" = String, Path, description = "Project id (a room in the API)")),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The project's rounds, oldest first", body = [RoundOut]),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such room, or the caller is not a member", body = Detail),
    )
)]
async fn list_rounds(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
) -> Result<Json<Vec<RoundOut>>, ApiError> {
    membership(&state, &room_id, user.id).await?;
    let mut out = Vec::new();
    for round in rounds::rounds_of(&state.pool, &room_id).await? {
        out.push(present(&state, round, user.id).await?);
    }
    Ok(Json(out))
}

#[utoipa::path(
    post,
    path = "/api/rooms/{room_id}/rounds",
    params(("room_id" = String, Path, description = "Project id (a room in the API)")),
    request_body = NewRound,
    security(("cookie" = [])),
    responses(
        (status = 201, description = "The new round, proposing; the caller decides it", body = RoundOut),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller is a viewer", body = Detail),
        (status = 404, description = "No such room, or the caller is not a member", body = Detail),
        (status = 409, description = "A public or personal project has no rounds", body = Detail),
        (status = 422, description = "The name, deadline or budget is invalid", body = Detail),
    )
)]
async fn create_round(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
    JsonBody(body): JsonBody<NewRound>,
) -> Result<(StatusCode, Json<RoundOut>), ApiError> {
    lifecycle::require_role(&state, &room_id, user.id, Role::Editor).await?;
    lifecycle::refuse_public(&state, &room_id).await?;
    lifecycle::refuse_personal(&state, &room_id).await?;
    let name = clean_name(&body.name)?;
    let deadline = body.deadline.as_deref().map(checked_deadline).transpose()?;
    let budget = body.budget.as_ref().map(stored_budget).transpose()?;
    let kind = body.kind.unwrap_or(match budget {
        Some(_) => Kind::Downlink,
        None => Kind::Labelling,
    });
    let id = rounds::create_round(
        &state.pool,
        &room_id,
        RoundSpec {
            name,
            kind,
            visibility: body.visibility,
            deadline,
            budget,
        },
        user.id,
    )
    .await?;
    let row = rounds::round(&state.pool, id)
        .await?
        .ok_or(ApiError::Internal)?;
    Ok((
        StatusCode::CREATED,
        Json(present(&state, row, user.id).await?),
    ))
}

#[utoipa::path(
    get,
    path = "/api/rooms/{room_id}/rounds/{round}",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("round" = String, Path, description = "Round uuid"),
    ),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The round", body = RoundOut),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such room or round, or the caller is not a member", body = Detail),
    )
)]
async fn get_round(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, round)): ApiPath<(String, String)>,
) -> Result<Json<RoundOut>, ApiError> {
    let (row, _) = load(&state, &room_id, &round, user.id).await?;
    Ok(Json(present(&state, row, user.id).await?))
}

fn patch_of(change: RoundChange) -> Result<RoundPatch, ApiError> {
    Ok(RoundPatch {
        name: change.name.as_deref().map(clean_name).transpose()?,
        kind: change.kind,
        visibility: change.visibility,
        deadline: change
            .deadline
            .map(|d| d.as_deref().map(checked_deadline).transpose())
            .transpose()?,
        budget: change
            .budget
            .map(|b| b.as_ref().map(stored_budget).transpose())
            .transpose()?,
    })
}

#[utoipa::path(
    patch,
    path = "/api/rooms/{room_id}/rounds/{round}",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("round" = String, Path, description = "Round uuid"),
    ),
    request_body = RoundChange,
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The changed round", body = RoundOut),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller neither decides the round nor owns the project", body = Detail),
        (status = 404, description = "No such room or round, or the caller is not a member", body = Detail),
        (status = 409, description = "The round is decided", body = Detail),
        (status = 422, description = "The name, deadline or budget is invalid", body = Detail),
    )
)]
async fn patch_round(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, round)): ApiPath<(String, String)>,
    JsonBody(change): JsonBody<RoundChange>,
) -> Result<Json<RoundOut>, ApiError> {
    let (row, seat) = load(&state, &room_id, &round, user.id).await?;
    require_administrator(&seat)?;
    refuse_decided(&row)?;
    rounds::update_round(&state.pool, row.id, patch_of(change)?).await?;
    ws::check_room_roles(&state, &room_id).await;
    Ok(Json(reload(&state, row.id, user.id).await?))
}

async fn reload(state: &AppState, id: Uuid, viewer: Uuid) -> Result<RoundOut, ApiError> {
    let row = rounds::round(&state.pool, id)
        .await?
        .ok_or(ApiError::Internal)?;
    present(state, row, viewer).await
}

async fn resolve(
    state: &AppState,
    room_id: &str,
    emails: &[String],
    role: RoundRole,
) -> Result<Vec<(Uuid, RoundRole)>, ApiError> {
    let mut out = Vec::new();
    let unique: std::collections::BTreeSet<_> = emails.iter().collect();
    for email in unique {
        let member = db::user_by_email(&state.pool, email).await?;
        let project_role = match &member {
            Some(user) => db::role_in(&state.pool, room_id, user.id).await?,
            None => None,
        };
        let (Some(member), Some(project_role)) = (member, project_role) else {
            return Err(unprocessable(format!(
                "{email} is not a member of the project"
            )));
        };
        if role == RoundRole::Decider && !project_role.can_write() {
            return Err(unprocessable(format!(
                "{email} must be an owner or editor to decide"
            )));
        }
        out.push((member.id, role));
    }
    Ok(out)
}

#[utoipa::path(
    put,
    path = "/api/rooms/{room_id}/rounds/{round}/members",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("round" = String, Path, description = "Round uuid"),
    ),
    request_body = RoundMembers,
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The round with its new members", body = RoundOut),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller neither decides the round nor owns the project", body = Detail),
        (status = 404, description = "No such room or round, or the caller is not a member", body = Detail),
        (status = 422, description = "An address is not a project member, or a decider cannot edit", body = Detail),
    )
)]
async fn put_members(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, round)): ApiPath<(String, String)>,
    JsonBody(body): JsonBody<RoundMembers>,
) -> Result<Json<RoundOut>, ApiError> {
    let (row, seat) = load(&state, &room_id, &round, user.id).await?;
    require_administrator(&seat)?;
    refuse_decided(&row)?;
    let mut members = resolve(&state, &room_id, &body.proposers, RoundRole::Proposer).await?;
    members.extend(resolve(&state, &room_id, &body.voters, RoundRole::Voter).await?);
    members.extend(resolve(&state, &room_id, &body.deciders, RoundRole::Decider).await?);
    rounds::set_members(&state.pool, row.id, &members).await?;
    ws::check_room_roles(&state, &room_id).await;
    Ok(Json(reload(&state, row.id, user.id).await?))
}

async fn catalogue_exists(state: &AppState, room_id: &str, uuid: &str) -> Result<bool, ApiError> {
    let id = uuid.to_owned();
    let found = state
        .rooms
        .read(room_id, move |doc| {
            model::catalogue(&doc.transact(), &id).is_some_and(|c| !c.deleted)
        })
        .await?;
    Ok(found)
}

async fn outcome<'a>(
    state: &AppState,
    room_id: &str,
    change: &'a Move,
) -> Result<Option<&'a str>, ApiError> {
    if change.state != RoundState::Decided {
        return Ok(None);
    }
    let catalogue = change
        .final_catalogue
        .as_deref()
        .ok_or_else(|| unprocessable("deciding needs the final catalogue"))?;
    match catalogue_exists(state, room_id, catalogue).await? {
        true => Ok(Some(catalogue)),
        false => Err(unprocessable(format!(
            "Catalogue {catalogue} is not in this project"
        ))),
    }
}

#[utoipa::path(
    post,
    path = "/api/rooms/{room_id}/rounds/{round}/state",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("round" = String, Path, description = "Round uuid"),
    ),
    request_body = Move,
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The moved round, with the move in its log", body = RoundOut),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller may not make this move in this round", body = Detail),
        (status = 404, description = "No such room or round, or the caller is not a member", body = Detail),
        (status = 409, description = "The round changed state since it was read", body = Detail),
        (status = 422, description = "Deciding needs a catalogue of the project", body = Detail),
    )
)]
async fn move_round(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, round)): ApiPath<(String, String)>,
    JsonBody(change): JsonBody<Move>,
) -> Result<Json<RoundOut>, ApiError> {
    let note = checked_note(change.note.as_deref())?;
    let (row, seat) = load(&state, &room_id, &round, user.id).await?;
    if !rules::may_move(&row, &seat, change.state) {
        return Err(forbidden("You may not make this move in this round."));
    }
    let catalogue = outcome(&state, &room_id, &change).await?;
    if let Some(catalogue) = catalogue {
        stamp_round(&state, &row, catalogue, user.id).await?;
    }
    let moved = rounds::set_state(
        &state.pool,
        row.id,
        row.state,
        change.state,
        catalogue,
        Some(user.id),
        note,
    )
    .await?;
    if !moved {
        return Err(ApiError::Conflict(
            "The round changed state meanwhile; reload it".to_owned(),
        ));
    }
    ws::check_room_roles(&state, &room_id).await;
    Ok(Json(reload(&state, row.id, user.id).await?))
}

fn round_stamp(round: &RoundRow, decider: &str, proposers: Vec<String>, at: String) -> Value {
    let budget = round
        .budget
        .as_deref()
        .and_then(|budget| serde_json::from_str::<Value>(budget).ok());
    let mut stamp = json!({
        "@context": {"cocat": "https://cocat.sciqlop.fr/ns/round#"},
        "@type": "cocat:Round",
        "cocat:name": round.name,
        "cocat:decidedBy": decider,
        "cocat:decidedAt": at,
        "cocat:proposers": proposers,
    });
    if let Some(budget) = budget {
        stamp["cocat:budget"] = budget;
    }
    stamp
}

/// The update that sets the catalogue's `round` attribute, applied on the room as it is.
async fn stamp_update(
    state: &AppState,
    room_id: &str,
    catalogue: &str,
    stamp: &Value,
) -> Result<Vec<u8>, ApiError> {
    let uuid = catalogue.to_owned();
    let attributes = Map::from_iter([("round".to_owned(), Value::String(stamp.to_string()))]);
    let update = state
        .rooms
        .read_log(room_id, 1, move |records, _| {
            let present = replay_until(records, u64::MAX);
            let mut txn = present.transact_mut();
            model::set_catalogue_attributes(&mut txn, &uuid, &attributes)
                .map(|_| txn.encode_update_v1())
        })
        .await?;
    update.map_err(|_| ApiError::Internal)
}

/// Records the round on the final catalogue, as the decider's own write.
async fn stamp_round(
    state: &AppState,
    round: &RoundRow,
    catalogue: &str,
    decider: Uuid,
) -> Result<(), ApiError> {
    let members = rounds::members(&state.pool, round.id).await?;
    let emails = Emails::load(state, members.iter().map(|(id, _)| *id).chain([decider])).await?;
    let at = Timestamp::now().rfc3339().map_err(|_| ApiError::Internal)?;
    let stamp = round_stamp(
        round,
        &emails.of(decider),
        emails.with_role(&members, RoundRole::Proposer),
        at,
    );
    let update = stamp_update(state, &round.room_id, catalogue, &stamp).await?;
    let allowance = state.rooms.allowance(&state.pool, &round.room_id).await?;
    state
        .rooms
        .write_as(&round.room_id, allowance, Author::Client(decider), update)
        .await?;
    Ok(())
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct MyProposal {
    /// The proposal's document: its socket is `/room/{room_id}/{doc}`.
    doc: Uuid,
}

fn may_start_proposal(round: &RoundRow, seat: &Seat) -> Result<(), ApiError> {
    let proposing = round.state == RoundState::Proposing;
    match proposing && seat.roles.contains(&RoundRole::Proposer) {
        true => Ok(()),
        false => Err(forbidden(
            "Only a proposer of the round gets a proposal, while it is proposing",
        )),
    }
}

#[utoipa::path(
    post,
    path = "/api/rooms/{room_id}/rounds/{round}/proposal",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("round" = String, Path, description = "Round uuid"),
    ),
    security(("cookie" = [])),
    responses(
        (status = 201, description = "The caller's new, empty proposal", body = MyProposal),
        (status = 200, description = "The caller's proposal, made earlier", body = MyProposal),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller has no proposal and either is no proposer or the round is past proposing", body = Detail),
        (status = 404, description = "No such room or round, or the caller is not a member", body = Detail),
    )
)]
async fn my_proposal(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, round)): ApiPath<(String, String)>,
) -> Result<(StatusCode, Json<MyProposal>), ApiError> {
    let (row, seat) = load(&state, &room_id, &round, user.id).await?;
    if let Some(doc) = rounds::proposal_of(&state.pool, row.id, user.id).await? {
        return Ok((StatusCode::OK, Json(MyProposal { doc })));
    }
    may_start_proposal(&row, &seat)?;
    let doc = rounds::add_proposal(&state.pool, row.id, user.id).await?;
    Ok((StatusCode::CREATED, Json(MyProposal { doc })))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct VotesIn {
    events: Vec<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct VotesOut {
    voter: String,
    events: Vec<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct VerdictsIn {
    /// Event uuid to its verdict, or `null` to clear it; events not listed are left alone.
    #[schema(value_type = Object)]
    verdicts: BTreeMap<String, Option<Verdict>>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct VerdictIn {
    verdict: Option<Verdict>,
}

#[derive(Debug, Serialize, PartialEq, utoipa::ToSchema)]
struct ReviewerVerdict {
    reviewer: String,
    verdict: Verdict,
    revision: i64,
}

#[derive(Debug, Serialize, PartialEq, utoipa::ToSchema)]
struct EventVerdicts {
    event: String,
    accept: u32,
    reject: u32,
    unsure: u32,
    by: Vec<ReviewerVerdict>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct VerdictsOut {
    revision: i64,
    events: Vec<EventVerdicts>,
}

fn may_vote(round: &RoundRow, seat: &Seat) -> Result<(), ApiError> {
    let voting = round.state == RoundState::Voting;
    match voting && seat.roles.contains(&RoundRole::Voter) {
        true => Ok(()),
        false => Err(forbidden(
            "Only a voter of the round votes, while it is voting",
        )),
    }
}

fn checked_event(event: &str) -> Result<String, ApiError> {
    Uuid::parse_str(event)
        .map(|uuid| uuid.to_string())
        .map_err(|_| unprocessable(format!("{event} is not an event uuid")))
}

async fn store_verdicts(
    state: &AppState,
    ids: (&str, &str),
    user: Uuid,
    changes: Vec<(String, Option<Verdict>)>,
) -> Result<StatusCode, ApiError> {
    let (room_id, round) = ids;
    let (row, seat) = load(state, room_id, round, user).await?;
    may_vote(&row, &seat)?;
    rounds::set_verdicts(&state.pool, row.id, user, row.revision, &changes).await?;
    Ok(StatusCode::OK)
}

#[utoipa::path(
    put,
    path = "/api/rooms/{room_id}/rounds/{round}/verdicts",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("round" = String, Path, description = "Round uuid"),
    ),
    request_body = VerdictsIn,
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The caller's verdicts were merged in"),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller is no voter of the round, or it is not voting", body = Detail),
        (status = 404, description = "No such room or round, or the caller is not a member", body = Detail),
        (status = 422, description = "An event id is not a uuid", body = Detail),
    )
)]
async fn put_verdicts(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, round)): ApiPath<(String, String)>,
    JsonBody(body): JsonBody<VerdictsIn>,
) -> Result<StatusCode, ApiError> {
    let changes = body
        .verdicts
        .into_iter()
        .map(|(event, verdict)| Ok((checked_event(&event)?, verdict)))
        .collect::<Result<Vec<_>, ApiError>>()?;
    store_verdicts(&state, (&room_id, &round), user.id, changes).await
}

#[utoipa::path(
    put,
    path = "/api/rooms/{room_id}/rounds/{round}/verdicts/{event}",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("round" = String, Path, description = "Round uuid"),
        ("event" = String, Path, description = "Event uuid"),
    ),
    request_body = VerdictIn,
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The caller's verdict on the event was set"),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller is no voter of the round, or it is not voting", body = Detail),
        (status = 404, description = "No such room or round, or the caller is not a member", body = Detail),
        (status = 422, description = "The event id is not a uuid", body = Detail),
    )
)]
async fn put_verdict(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, round, event)): ApiPath<(String, String, String)>,
    JsonBody(body): JsonBody<VerdictIn>,
) -> Result<StatusCode, ApiError> {
    let changes = vec![(checked_event(&event)?, body.verdict)];
    store_verdicts(&state, (&room_id, &round), user.id, changes).await
}

/// Counts and reviewers per event; only current-revision verdicts are counted.
fn summarize(rows: Vec<(String, VerdictRow)>, revision: i64) -> Vec<EventVerdicts> {
    let mut events: BTreeMap<String, EventVerdicts> = BTreeMap::new();
    for (email, row) in rows {
        let entry = events
            .entry(row.event.clone())
            .or_insert_with(|| EventVerdicts {
                event: row.event.clone(),
                accept: 0,
                reject: 0,
                unsure: 0,
                by: Vec::new(),
            });
        if row.revision == revision {
            match row.verdict {
                Verdict::Accept => entry.accept += 1,
                Verdict::Reject => entry.reject += 1,
                Verdict::Unsure => entry.unsure += 1,
            }
        }
        entry.by.push(ReviewerVerdict {
            reviewer: email,
            verdict: row.verdict,
            revision: row.revision,
        });
    }
    events.into_values().collect()
}

#[utoipa::path(
    get,
    path = "/api/rooms/{room_id}/rounds/{round}/verdicts",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("round" = String, Path, description = "Round uuid"),
    ),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The verdicts the caller may read, counted per event", body = VerdictsOut),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such room or round, or the caller is not a member", body = Detail),
    )
)]
async fn get_verdicts(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, round)): ApiPath<(String, String)>,
) -> Result<Json<VerdictsOut>, ApiError> {
    let (row, seat) = load(&state, &room_id, &round, user.id).await?;
    let readable: Vec<_> = rounds::verdicts(&state.pool, row.id)
        .await?
        .into_iter()
        .filter(|verdict| rules::may_read_verdicts(&row, &seat, verdict.reviewer == user.id))
        .collect();
    let emails = Emails::load(&state, readable.iter().map(|verdict| verdict.reviewer)).await?;
    let tagged = readable
        .into_iter()
        .map(|verdict| (emails.of(verdict.reviewer), verdict))
        .collect();
    Ok(Json(VerdictsOut {
        revision: row.revision,
        events: summarize(tagged, row.revision),
    }))
}

#[utoipa::path(
    put,
    path = "/api/rooms/{room_id}/rounds/{round}/votes",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("round" = String, Path, description = "Round uuid"),
    ),
    request_body = VotesIn,
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The caller accepts exactly these events; rejects and unsures stay"),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller is no voter of the round, or it is not voting", body = Detail),
        (status = 404, description = "No such room or round, or the caller is not a member", body = Detail),
    )
)]
async fn put_votes(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, round)): ApiPath<(String, String)>,
    JsonBody(body): JsonBody<VotesIn>,
) -> Result<StatusCode, ApiError> {
    let (row, seat) = load(&state, &room_id, &round, user.id).await?;
    may_vote(&row, &seat)?;
    rounds::set_accepts(&state.pool, &row, user.id, &body.events).await?;
    Ok(StatusCode::OK)
}

fn accepts_by_voter(rows: Vec<(String, VerdictRow)>, revision: i64) -> Vec<VotesOut> {
    let mut by_voter: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (email, row) in rows {
        if row.verdict == Verdict::Accept && row.revision == revision {
            by_voter.entry(email).or_default().push(row.event);
        }
    }
    by_voter
        .into_iter()
        .map(|(voter, events)| VotesOut { voter, events })
        .collect()
}

#[utoipa::path(
    get,
    path = "/api/rooms/{room_id}/rounds/{round}/votes",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("round" = String, Path, description = "Round uuid"),
    ),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The accepts the caller may read, per voter", body = [VotesOut]),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such room or round, or the caller is not a member", body = Detail),
    )
)]
async fn get_votes(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, round)): ApiPath<(String, String)>,
) -> Result<Json<Vec<VotesOut>>, ApiError> {
    let (row, seat) = load(&state, &room_id, &round, user.id).await?;
    let readable: Vec<_> = rounds::verdicts(&state.pool, row.id)
        .await?
        .into_iter()
        .filter(|verdict| rules::may_read_verdicts(&row, &seat, verdict.reviewer == user.id))
        .collect();
    let emails = Emails::load(&state, readable.iter().map(|verdict| verdict.reviewer)).await?;
    let tagged = readable
        .into_iter()
        .map(|verdict| (emails.of(verdict.reviewer), verdict))
        .collect();
    Ok(Json(accepts_by_voter(tagged, row.revision)))
}
