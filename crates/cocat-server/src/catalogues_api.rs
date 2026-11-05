//! The read-only catalogue routes under `/api`.
//!
//! They answer from a room's document without opening it as a live room, so a script
//! paging through a 100k-event catalogue never costs a client's worth of memory.

use axum::Json;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use cocat_core::model::{self, CatalogueSummary, EventRecord};
use cocat_core::time::Timestamp;
use cocat_core::votable;
use serde::{Deserialize, Serialize};
use yrs::Transact;

use crate::AppState;
use crate::api::{ApiError, ApiPath, CurrentUser, membership};

const VOTABLE_SUFFIX: &str = ".votable";
const DEFAULT_LIMIT: usize = 1000;
const MAX_LIMIT: usize = 10_000;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/rooms/{room_id}/catalogues", get(list))
        .route("/rooms/{room_id}/catalogues/{catalogue}", get(one))
        .route(
            "/rooms/{room_id}/catalogues/{catalogue}/events",
            get(events),
        )
}

#[derive(Debug, Serialize)]
struct CatalogueList {
    catalogues: Vec<CatalogueSummary>,
}

#[derive(Debug, Serialize)]
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
            model::catalogue_events(&txn, &id).map(|events| range.page(events))
        })
        .await?;
    found.map(Json).ok_or_else(|| unknown(&catalogue))
}

async fn votable(state: &AppState, room_id: &str, uuid: &str) -> Result<Response, ApiError> {
    let id = uuid.to_owned();
    let exported = state
        .rooms
        .read(room_id, move |doc| {
            let txn = doc.transact();
            model::catalogue(&txn, &id).map(|summary| {
                let events = model::catalogue_events(&txn, &id).unwrap_or_default();
                votable::export(&summary, &events)
            })
        })
        .await?
        .ok_or_else(|| unknown(uuid))?
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
