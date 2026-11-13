//! `GET /api/public/events`: one search through the events of every public catalogue, the
//! scope a smart catalogue takes to watch them all.
//!
//! Reading each room file for every search would take seconds, so the events of each public
//! catalogue are kept in memory as records, read again only when its file changed.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Query, State};
use cocat_core::model::{self, EventRecord};
use cocat_core::rules::{Lookups, Predicate};
use cocat_core::time::Timestamp;
use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use yrs::Transact;

use crate::AppState;
use crate::api::{ApiError, CurrentUser};
use crate::db;
use crate::openapi::Detail;
use crate::public_api::file_stamp;

const DEFAULT_LIMIT: usize = 1000;
const MAX_LIMIT: usize = 10_000;
/// How many public catalogues are read at once when they are not in memory yet; the room
/// manager limits the parsing itself.
const LOADS: usize = 8;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(events))
}

/// The events of each public catalogue as last read, with the file stamp they were read at.
// simplify: every public event stays in memory once searched (about a million records on the
// dev server, a few hundred MB). Upgrade path: an on-disk index (SQLite) when that shows.
#[derive(Debug, Clone, Default)]
pub struct PublicEvents(Arc<Mutex<HashMap<String, Kept>>>);

/// A catalogue's events, and the stamp of the file they were read from.
type Kept = (String, Arc<Vec<EventRecord>>);

impl PublicEvents {
    fn cached(&self, room_id: &str, stamp: &str) -> Option<Arc<Vec<EventRecord>>> {
        let rooms = self.0.lock().ok()?;
        let (kept_at, events) = rooms.get(room_id)?;
        (kept_at == stamp).then(|| Arc::clone(events))
    }

    fn keep(&self, room_id: &str, stamp: String, events: Arc<Vec<EventRecord>>) {
        if let Ok(mut rooms) = self.0.lock() {
            rooms.insert(room_id.to_owned(), (stamp, events));
        }
    }

    /// Forgets the catalogues that are no longer public.
    fn retain(&self, public: &HashSet<&str>) {
        if let Ok(mut rooms) = self.0.lock() {
            rooms.retain(|room_id, _| public.contains(room_id.as_str()));
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct EventsQuery {
    q: Option<String>,
    limit: Option<String>,
}

/// The first events found, in time order, and how many there are in all.
#[derive(Debug, Serialize, utoipa::ToSchema)]
struct Found {
    events: Vec<PublicEvent>,
    total: usize,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct PublicEvent {
    /// The public catalogue that holds it: its id, as `/api/public` lists it.
    catalogue: String,
    #[serde(flatten)]
    event: EventRecord,
}

#[utoipa::path(
    get,
    path = "/api/public/events",
    params(
        ("q" = String, Query, description = "A search, as the search box takes it; `catalogue:\"…\"` names a public catalogue"),
        ("limit" = Option<u32>, Query, minimum = 1, maximum = 10000, description = "How many events to return, 1000 by default and 10000 at most"),
    ),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The events found in the public catalogues", body = Found),
        (status = 401, description = "No valid session", body = Detail),
        (status = 422, description = "The search does not read, or the limit is not a number", body = Detail),
    )
)]
async fn events(
    State(state): State<AppState>,
    CurrentUser(_): CurrentUser,
    Query(query): Query<EventsQuery>,
) -> Result<Json<Found>, ApiError> {
    let limit = limit(query.limit.as_deref())?;
    let rooms = db::public_rooms(&state.pool).await?;
    let names: Vec<(String, String)> = rooms
        .iter()
        .map(|room| (room.id.clone(), room.name.clone()))
        .collect();
    let text = query.q.unwrap_or_default();
    let predicate = cocat_core::query::parse(&text, &names)
        .map_err(|error| ApiError::Unprocessable(error.to_string()))?;
    let loaded = loaded(&state, &names).await;
    let found = tokio::task::spawn_blocking(move || search(&loaded, &predicate, limit))
        .await
        .map_err(|_| ApiError::Internal)??;
    Ok(Json(found))
}

fn limit(text: Option<&str>) -> Result<usize, ApiError> {
    match text {
        None => Ok(DEFAULT_LIMIT),
        Some(text) => text
            .parse::<usize>()
            .ok()
            .filter(|limit| (1..=MAX_LIMIT).contains(limit))
            .ok_or_else(|| {
                ApiError::Unprocessable(format!("limit is a number from 1 to {MAX_LIMIT}"))
            }),
    }
}

/// The events of each public catalogue; one that cannot be read is left out and logged.
async fn loaded(
    state: &AppState,
    rooms: &[(String, String)],
) -> Vec<(String, Arc<Vec<EventRecord>>)> {
    let ids: HashSet<&str> = rooms.iter().map(|(id, _)| id.as_str()).collect();
    state.rooms.public_events().retain(&ids);
    let loads: Vec<_> = rooms.iter().map(|(room_id, _)| {
        // Owned, so the futures name no borrowed lifetime the handler would have to be
        // general over.
        let (state, room_id) = (state.clone(), room_id.clone());
        async move {
            match room_events(&state, &room_id).await {
                Ok(events) => Some((room_id, events)),
                Err(error) => {
                    tracing::warn!(room = %room_id, %error, "a public catalogue was left out of a search");
                    None
                }
            }
        }
    }).collect();
    stream::iter(loads)
        .buffer_unordered(LOADS)
        .filter_map(std::future::ready)
        .collect()
        .await
}

async fn room_events(
    state: &AppState,
    room_id: &str,
) -> Result<Arc<Vec<EventRecord>>, crate::room::RoomError> {
    let cache = state.rooms.public_events();
    let stamp = file_stamp(state, room_id);
    if let Some(events) = cache.cached(room_id, &stamp) {
        return Ok(events);
    }
    let everything = Predicate::And(Vec::new());
    let events = state
        .rooms
        .read(room_id, move |doc| {
            model::search(&doc.transact(), &everything, Timestamp::now()).unwrap_or_default()
        })
        .await?;
    let events = Arc::new(events);
    cache.keep(room_id, stamp, Arc::clone(&events));
    Ok(events)
}

/// The events `predicate` finds, the first `limit` of them in time order.
fn search(
    rooms: &[(String, Arc<Vec<EventRecord>>)],
    predicate: &Predicate,
    limit: usize,
) -> Result<Found, ApiError> {
    let lookups = Lookups::new(predicate, Timestamp::now(), &mut |_| Ok(HashSet::new()))
        .map_err(|error| ApiError::Unprocessable(error.to_string()))?;
    let lookups = &lookups;
    let mut found: Vec<(&str, &EventRecord)> = rooms
        .iter()
        .flat_map(|(room_id, events)| {
            let predicate = in_room(predicate, room_id);
            events
                .iter()
                .filter(move |event| model::record_matches(&predicate, lookups, event))
                .map(move |event| (room_id.as_str(), event))
        })
        .collect();
    let total = found.len();
    found.sort_by(|(_, a), (_, b)| (a.start, a.stop, &a.uuid).cmp(&(b.start, b.stop, &b.uuid)));
    let events = found
        .into_iter()
        .take(limit)
        .map(|(catalogue, event)| PublicEvent {
            catalogue: catalogue.to_owned(),
            event: event.clone(),
        })
        .collect();
    Ok(Found { events, total })
}

/// `predicate` for the events of one public catalogue: `catalogue:"…"` asks whether it is
/// that catalogue, and every public event is listed by its catalogue.
fn in_room(predicate: &Predicate, room_id: &str) -> Predicate {
    let always = |yes: bool| match yes {
        true => Predicate::And(Vec::new()),
        false => Predicate::Or(Vec::new()),
    };
    let inner = |all: &[Predicate]| all.iter().map(|p| in_room(p, room_id)).collect();
    match predicate {
        Predicate::InCatalogue(catalogue) => always(catalogue.as_deref() == Some(room_id)),
        Predicate::And(all) => Predicate::And(inner(all)),
        Predicate::Or(any) => Predicate::Or(inner(any)),
        Predicate::Not(one) => Predicate::Not(Box::new(in_room(one, room_id))),
        other => other.clone(),
    }
}
