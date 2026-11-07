//! The read-only catalogue routes under `/api`.
//!
//! They answer from a room's document without opening it as a live room, so a script
//! paging through a 100k-event catalogue never costs a client's worth of memory.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use cocat_core::model::{self, CatalogueSummary, EventRecord};
use cocat_core::time::Timestamp;
use cocat_core::votable;
use serde::{Deserialize, Serialize};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use yrs::Transact;

use crate::AppState;
use crate::api::{ApiError, ApiPath, CurrentUser, membership};
use crate::openapi::Detail;

const VOTABLE_SUFFIX: &str = ".votable";
const DEFAULT_LIMIT: usize = 1000;
const MAX_LIMIT: usize = 10_000;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list))
        .routes(routes!(one))
        .routes(routes!(events))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct CatalogueList {
    catalogues: Vec<CatalogueSummary>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct EventsPage {
    events: Vec<EventRecord>,
    next: Option<String>,
}

/// The query strings stay strings: a value that does not parse must be the 422 the plan
/// promises, not axum's own 400 rejection.
#[derive(Debug, Default, Deserialize)]
struct EventsQuery {
    start: Option<String>,
    stop: Option<String>,
    limit: Option<String>,
    cursor: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/rooms/{room_id}/catalogues",
    params(("room_id" = String, Path, description = "Room id")),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The catalogues in the room", body = CatalogueList),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such room, or the caller is not a member", body = Detail),
    )
)]
async fn list(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
) -> Result<Json<CatalogueList>, ApiError> {
    membership(&state, &room_id, user.id).await?;
    let catalogues = state
        .rooms
        .read(&room_id, |doc| {
            let txn = doc.transact();
            model::catalogues(&txn)
        })
        .await?;
    Ok(Json(CatalogueList { catalogues }))
}

#[utoipa::path(
    get,
    path = "/api/rooms/{room_id}/catalogues/{catalogue}",
    params(
        ("room_id" = String, Path, description = "Room id"),
        ("catalogue" = String, Path, description = "Catalogue uuid; a .votable suffix answers VOTable XML"),
    ),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The catalogue, JSON or VOTable XML", body = CatalogueSummary),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such room, catalogue or membership", body = Detail),
    )
)]
async fn one(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, catalogue)): ApiPath<(String, String)>,
) -> Result<Response, ApiError> {
    membership(&state, &room_id, user.id).await?;
    if let Some(uuid) = catalogue.strip_suffix(VOTABLE_SUFFIX) {
        return votable(&state, &room_id, uuid).await;
    }
    let id = catalogue.clone();
    let found = state
        .rooms
        .read(&room_id, move |doc| {
            let txn = doc.transact();
            model::catalogue(&txn, &id)
        })
        .await?;
    match found {
        Some(summary) => Ok(Json(summary).into_response()),
        None => Err(unknown(&catalogue)),
    }
}

#[utoipa::path(
    get,
    path = "/api/rooms/{room_id}/catalogues/{catalogue}/events",
    params(
        ("room_id" = String, Path, description = "Room id"),
        ("catalogue" = String, Path, description = "Catalogue uuid"),
        ("start" = Option<String>, Query, format = DateTime, description = "Only events that stop at or after this time"),
        ("stop" = Option<String>, Query, format = DateTime, description = "Only events that start at or before this time"),
        ("limit" = Option<u32>, Query, minimum = 1, maximum = 10000, description = "Page size, 1000 by default and 10000 at most"),
        ("cursor" = Option<String>, Query, min_length = 1, description = "The page cursor returned by the previous call"),
    ),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "One page of events", body = EventsPage),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such room, catalogue or membership", body = Detail),
        (status = 422, description = "A query value does not parse, or the catalogue's rule does not evaluate", body = Detail),
    )
)]
async fn events(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, catalogue)): ApiPath<(String, String)>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<EventsPage>, ApiError> {
    membership(&state, &room_id, user.id).await?;
    let range = Range::parse(&query)?;
    let id = catalogue.clone();
    let found = state
        .rooms
        .read(&room_id, move |doc| {
            let txn = doc.transact();
            model::catalogue_events(&txn, &id)
        })
        .await?
        .ok_or_else(|| unknown(&catalogue))?
        .map_err(|error| ApiError::Unprocessable(error.to_string()))?;
    Ok(Json(range.page(found)))
}

async fn votable(state: &AppState, room_id: &str, uuid: &str) -> Result<Response, ApiError> {
    let id = uuid.to_owned();
    let exported = state
        .rooms
        .read(room_id, move |doc| {
            let txn = doc.transact();
            let summary = model::catalogue(&txn, &id)?;
            let events = model::catalogue_events(&txn, &id)?;
            Some(events.map(|events| votable::export(&summary, &events)))
        })
        .await?
        .ok_or_else(|| unknown(uuid))?
        .map_err(|error| ApiError::Unprocessable(error.to_string()))?
        .map_err(|error| ApiError::Unprocessable(error.to_string()))?;

    let mut response = exported.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-votable+xml"),
    );
    let disposition = HeaderValue::try_from(format!("attachment; filename=\"{uuid}.xml\""))
        .map_err(|_| ApiError::Internal)?;
    headers.insert(header::CONTENT_DISPOSITION, disposition);
    Ok(response)
}

fn unknown(uuid: &str) -> ApiError {
    ApiError::NotFound(format!("Catalogue {uuid} not found"))
}

fn invalid(detail: &str) -> ApiError {
    ApiError::Unprocessable(detail.to_owned())
}

/// The events of one page: the time window, the page size and where the last page stopped.
struct Range {
    start: Option<Timestamp>,
    stop: Option<Timestamp>,
    limit: usize,
    after: Option<(i128, String)>,
}

impl Range {
    fn parse(query: &EventsQuery) -> Result<Self, ApiError> {
        let start = query.start.as_deref().map(instant).transpose()?;
        let stop = query.stop.as_deref().map(instant).transpose()?;
        if let (Some(start), Some(stop)) = (start, stop)
            && start > stop
        {
            return Err(invalid("the start must not be after the stop"));
        }
        let limit = match query.limit.as_deref() {
            None => DEFAULT_LIMIT,
            Some(text) => match text.parse::<usize>() {
                Ok(limit) if (1..=MAX_LIMIT).contains(&limit) => limit,
                _ => return Err(invalid("the limit must be between 1 and 10000")),
            },
        };
        let after = query.cursor.as_deref().map(cursor).transpose()?;
        Ok(Self {
            start,
            stop,
            limit,
            after,
        })
    }

    /// An event belongs to the window when the two intervals overlap, either bound open.
    fn covers(&self, event: &EventRecord) -> bool {
        self.start.is_none_or(|start| event.stop >= start)
            && self.stop.is_none_or(|stop| event.start <= stop)
    }

    // simplify: every page filters and sorts the whole catalogue and a closed room is
    // parsed again per page; add a short-lived read cache when the UI pages through big
    // closed rooms.
    fn page(self, events: Vec<EventRecord>) -> EventsPage {
        let mut selected: Vec<EventRecord> = events
            .into_iter()
            .filter(|event| self.covers(event))
            .filter(|event| match &self.after {
                None => true,
                Some((micros, uuid)) => (event.start.unix_micros(), &event.uuid) > (*micros, uuid),
            })
            .collect();
        let more = selected.len() > self.limit;
        selected.truncate(self.limit);
        let next = if more {
            selected.last().map(position)
        } else {
            None
        };
        EventsPage {
            events: selected,
            next,
        }
    }
}

fn instant(text: &str) -> Result<Timestamp, ApiError> {
    Timestamp::parse(text).ok_or_else(|| invalid("the times must be ISO 8601 instants"))
}

/// A cursor names the last event of a page, so the next page starts strictly after it.
fn position(event: &EventRecord) -> String {
    URL_SAFE_NO_PAD.encode(format!("{}|{}", event.start.unix_micros(), event.uuid))
}

fn cursor(text: &str) -> Result<(i128, String), ApiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| invalid("the cursor is not valid"))?;
    let text = String::from_utf8(bytes).map_err(|_| invalid("the cursor is not valid"))?;
    let (micros, uuid) = text
        .split_once('|')
        .ok_or_else(|| invalid("the cursor is not valid"))?;
    let micros = micros
        .parse()
        .map_err(|_| invalid("the cursor is not valid"))?;
    Ok((micros, uuid.to_owned()))
}
