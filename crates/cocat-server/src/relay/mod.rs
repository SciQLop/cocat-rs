//! The relay: public catalogues that follow a remote provider (DONKI, CDAW, AMDA). A run
//! fetches the provider's lists and makes each public catalogue hold exactly what the
//! provider publishes now, as one version authored by the provider's source. Nobody edits a
//! relayed catalogue by hand.

pub mod store;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cocat_core::model::{self, Counts, EventRecord, Wanted};
use cocat_core::room_id::{InvalidRoomId, public_id};
use tokio::task::JoinError;
use yrs::Transact;

use crate::AppState;
use crate::db::{self, DbError, PublicRoom};
use crate::history::diff::replay_until;
use crate::history_api::save_newest_if_changed;
use crate::room::{Author, RoomError};
use crate::ws;

const PROJECT_URL: &str = "https://github.com/SciQLop/cocat-rs";
const FETCH_TIMEOUT: Duration = Duration::from_secs(120);

/// One list a provider publishes: a public catalogue `public.<provider>.<key>` holding one
/// catalogue, uuid `catalogue`, and `events`, whose uuids stay the same across fetches.
#[derive(Debug, Clone, PartialEq)]
pub struct Fetched {
    pub key: String,
    /// Where it sits in the index, `/`-separated: "DONKI/Flares".
    pub folder: String,
    pub name: String,
    pub description: String,
    /// Where people read the list at its source.
    pub url: String,
    pub catalogue: String,
    pub events: Vec<EventRecord>,
}

#[async_trait]
pub trait Provider: Send + Sync {
    /// Lower-case, the middle part of its catalogues' ids: "donki".
    fn id(&self) -> &'static str;
    /// The name people read: "DONKI (NASA CCMC)".
    fn source(&self) -> &'static str;
    /// A cron line, in UTC.
    fn default_schedule(&self) -> &'static str;
    /// Every list the provider publishes now; the error is what the administrator reads.
    async fn fetch(
        &self,
        http: &reqwest::Client,
        settings: &serde_json::Value,
    ) -> Result<Vec<Fetched>, String>;
    /// An event the provider no longer lists that stays anyway (it fell outside the window
    /// the provider fetches).
    fn keep(&self, _event: &EventRecord) -> bool {
        false
    }
}

/// The providers this server knows.
pub fn providers() -> Vec<Arc<dyn Provider>> {
    Vec::new()
}

/// The client providers fetch with: a timeout per request, and a User-Agent naming cocat and
/// where to reach this server, or else the project.
pub fn http_client(public_url: Option<&str>) -> Result<reqwest::Client, reqwest::Error> {
    let contact = public_url
        .filter(|url| !url.is_empty())
        .unwrap_or(PROJECT_URL);
    reqwest::Client::builder()
        .user_agent(format!(
            "cocat-relay/{} (+{contact})",
            env!("CARGO_PKG_VERSION")
        ))
        .timeout(FETCH_TIMEOUT)
        .build()
}

/// What a run did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunReport {
    /// How many lists the provider published.
    pub catalogues: usize,
    pub counts: Counts,
    /// How many of its catalogues it no longer publishes.
    pub unpublished: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("no provider is called {0}")]
    Unknown(String),
    #[error("a run of this provider is going on already")]
    Running,
    #[error("the run stopped on a bug: {0}")]
    Panicked(String),
    #[error("the provider could not be read: {0}")]
    Fetch(String),
    #[error("the provider published nothing: the catalogues were kept as they were")]
    Empty,
    #[error("the stored settings are not JSON: {0}")]
    Settings(#[from] serde_json::Error),
    #[error(transparent)]
    Key(#[from] InvalidRoomId),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Room(#[from] RoomError),
}

/// Runs the provider once, unless a run of it is going on, and records how it went. A run
/// that fails, or finds nothing, leaves every catalogue it did not reach as it was.
pub async fn run(state: &AppState, provider: &str) -> Result<RunReport, RelayError> {
    let provider = state
        .providers
        .iter()
        .find(|known| known.id() == provider)
        .cloned()
        .ok_or_else(|| RelayError::Unknown(provider.to_owned()))?;
    if !store::start(&state.pool, provider.id()).await? {
        return Err(RelayError::Running);
    }
    // The run is a task of its own, so a caller that gives up never leaves it marked running.
    tokio::spawn(run_to_end(state.clone(), provider))
        .await
        .map_err(|error| RelayError::Panicked(panic_message(error)))?
}

/// Refreshes, then records the outcome whatever it was, a panic included.
async fn run_to_end(state: AppState, provider: Arc<dyn Provider>) -> Result<RunReport, RelayError> {
    let refreshed = tokio::spawn(refresh(state.clone(), provider.clone()))
        .await
        .unwrap_or_else(|error| Err(RelayError::Panicked(panic_message(error))));
    let result = match &refreshed {
        Ok(report) => report.as_json(),
        Err(error) => serde_json::json!({ "error": error.to_string() }),
    };
    let ok = refreshed.is_ok();
    if let Err(error) = store::finish(&state.pool, provider.id(), ok, &result.to_string()).await {
        tracing::error!(%error, provider = provider.id(), "the end of a relay run was not recorded");
    }
    refreshed
}

fn panic_message(error: JoinError) -> String {
    match error.try_into_panic() {
        Ok(panic) => panic
            .downcast_ref::<&str>()
            .map(|text| (*text).to_owned())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "no message".to_owned()),
        Err(error) => error.to_string(),
    }
}

async fn refresh(state: AppState, provider: Arc<dyn Provider>) -> Result<RunReport, RelayError> {
    let settings = serde_json::from_str(&store::settings(&state.pool, provider.id()).await?)?;
    let fetched = provider
        .fetch(&state.http, &settings)
        .await
        .map_err(RelayError::Fetch)?;
    if fetched.is_empty() {
        return Err(RelayError::Empty);
    }
    let rooms = fetched
        .iter()
        .map(|list| public_id(&format!("{}.{}", provider.id(), list.key)))
        .collect::<Result<Vec<_>, _>>()?;
    let mut counts = Counts::default();
    for (room, list) in rooms.iter().zip(fetched) {
        counts += relay_list(&state, &provider, room, list).await?;
    }
    let unpublished = store::unpublish_others(&state.pool, provider.id(), &rooms).await?;
    Ok(RunReport {
        catalogues: rooms.len(),
        counts,
        unpublished,
    })
}

/// Makes public catalogue `room` the provider's list: what the index says, then its events,
/// as one version named after the source when something changed.
async fn relay_list(
    state: &AppState,
    provider: &Arc<dyn Provider>,
    room: &str,
    list: Fetched,
) -> Result<Counts, RelayError> {
    place(state, provider.as_ref(), room, &list).await?;
    let wanted = Wanted {
        catalogue: list.catalogue,
        name: list.name,
        events: list.events,
    };
    let (counts, update) = held(state, provider.clone(), room, wanted).await?;
    let allowance = state.rooms.allowance(&state.pool, room).await?;
    if state
        .rooms
        .write_as(room, allowance, Author::Server, update)
        .await?
    {
        let name = format!("Refreshed from {}", provider.source());
        save_newest_if_changed(state, room, None, Some(&name), None).await?;
    }
    store::refreshed(&state.pool, room).await?;
    Ok(counts)
}

/// The public catalogue as the index shows it, made when missing, and following the
/// provider. Curators editing a catalogue the relay adopts lose their write access now.
async fn place(
    state: &AppState,
    provider: &dyn Provider,
    room: &str,
    list: &Fetched,
) -> Result<(), RelayError> {
    let public = PublicRoom {
        id: room.to_owned(),
        name: list.name.clone(),
        folder: Some(list.folder.clone()).filter(|folder| !folder.is_empty()),
        source: Some(provider.source().to_owned()),
        description: Some(list.description.clone()).filter(|text| !text.is_empty()),
        public_count: None,
        counted_file: None,
    };
    let followed = store::relayed(&state.pool, room).await?.is_some();
    let created = db::create_public_room(&state.pool, &public, None).await?;
    if !created {
        db::update_public_room(&state.pool, &public).await?;
    }
    store::adopt(&state.pool, room, provider.id(), &list.key, &list.url).await?;
    if !created && !followed {
        ws::check_room_roles(state, room).await;
    }
    Ok(())
}

/// What holding `wanted` changes in the room as stored: the counts and the update.
async fn held(
    state: &AppState,
    provider: Arc<dyn Provider>,
    room: &str,
    wanted: Wanted,
) -> Result<(Counts, Vec<u8>), RelayError> {
    Ok(state
        .rooms
        .read_log(room, 1, move |records, _| {
            let present = replay_until(records, u64::MAX);
            let mut txn = present.transact_mut();
            let counts = model::hold(&mut txn, &wanted, |event| provider.keep(event));
            (counts, txn.encode_update_v1())
        })
        .await?)
}

impl RunReport {
    fn as_json(&self) -> serde_json::Value {
        let counts = &self.counts;
        serde_json::json!({
            "catalogues": self.catalogues,
            "added": counts.added,
            "changed": counts.changed,
            "removed": counts.removed,
            "restored": counts.restored,
            "skipped": counts.skipped,
            "unpublished": self.unpublished,
        })
    }
}
