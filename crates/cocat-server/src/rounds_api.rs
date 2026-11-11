//! The HTTP side of selection rounds: settings, members, state moves and what each caller sees.

use std::collections::HashMap;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use cocat_core::model;
use cocat_core::role::Role;
use cocat_core::round::{budget_json, parse_budget};
use cocat_core::time::Timestamp;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;
use yrs::Transact;

use crate::api::{ApiError, ApiPath, CurrentUser, JsonBody, membership};
use crate::openapi::Detail;
use crate::rounds::rules::{self, Seat};
use crate::rounds::{self, RoundPatch, RoundRole, RoundRow, RoundState, Visibility};
use crate::{AppState, db, lifecycle, ws};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_rounds, create_round))
        .routes(routes!(get_round, patch_round))
        .routes(routes!(put_members))
        .routes(routes!(move_round))
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
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct RoundOut {
    id: Uuid,
    project: String,
    name: String,
    state: RoundState,
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
    let roles = seat_roles(&rounds::members(&state.pool, id).await?, user);
    Ok((
        row,
        Seat {
            project_role,
            roles,
        },
    ))
}

fn seat_roles(members: &[(Uuid, RoundRole)], user: Uuid) -> Vec<RoundRole> {
    members
        .iter()
        .filter(|(member, _)| *member == user)
        .map(|(_, role)| *role)
        .collect()
}

fn may_administer(seat: &Seat) -> bool {
    seat.project_role == Role::Owner || seat.roles.contains(&RoundRole::Decider)
}

fn require_administrator(seat: &Seat) -> Result<(), ApiError> {
    match may_administer(seat) {
        true => Ok(()),
        false => Err(forbidden("Only a decider or a project owner may do this")),
    }
}

fn stored_budget(budget: &Value) -> Result<String, ApiError> {
    let parsed = parse_budget(budget).map_err(|error| unprocessable(error.to_string()))?;
    Ok(budget_json(&parsed).to_string())
}

fn checked_deadline(deadline: &str) -> Result<String, ApiError> {
    Timestamp::parse(deadline)
        .map(|_| deadline.to_owned())
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
        roles: seat_roles(&members, viewer),
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
            })
            .collect(),
        budget: round
            .budget
            .as_deref()
            .and_then(|b| serde_json::from_str(b).ok()),
        id: round.id,
        project: round.room_id,
        name: round.name,
        state: round.state,
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
    let id = rounds::create_round(
        &state.pool,
        &room_id,
        &name,
        body.visibility,
        deadline,
        budget,
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
    if row.state == RoundState::Decided {
        return Err(ApiError::Conflict(
            "A decided round cannot be changed".to_owned(),
        ));
    }
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
    for email in emails {
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
        (status = 403, description = "Deciders move a round forward, only a project owner moves it back", body = Detail),
        (status = 404, description = "No such room or round, or the caller is not a member", body = Detail),
        (status = 422, description = "Deciding needs a catalogue of the project", body = Detail),
    )
)]
async fn move_round(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, round)): ApiPath<(String, String)>,
    JsonBody(change): JsonBody<Move>,
) -> Result<Json<RoundOut>, ApiError> {
    let (row, seat) = load(&state, &room_id, &round, user.id).await?;
    if !rules::may_move(&row, &seat, change.state) {
        return Err(forbidden(
            "Deciders move a round forward; only a project owner moves it back",
        ));
    }
    let catalogue = outcome(&state, &room_id, &change).await?;
    rounds::set_state(&state.pool, row.id, change.state, catalogue, Some(user.id)).await?;
    ws::check_room_roles(&state, &room_id).await;
    Ok(Json(reload(&state, row.id, user.id).await?))
}
