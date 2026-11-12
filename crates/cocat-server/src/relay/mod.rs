//! The relay: public catalogues that follow a remote provider (DONKI, CDAW, AMDA). A run
//! fetches the provider's lists and makes each public catalogue hold exactly what the
//! provider publishes now, as one version authored by the provider's source. Nobody edits a
//! relayed catalogue by hand.

pub mod admin;
pub mod amda;
pub mod cactus;
pub mod catcsv;
pub mod cdaw;
pub mod donki;
pub mod grmb;
pub mod helio4cast;
pub mod icmecat;
pub mod literature;
pub mod onera;
pub mod patient;
pub mod pmod;
pub mod rolling;
pub mod schedule;
pub mod stix;
pub mod store;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cocat_core::model::{self, Counts, EventRecord, Wanted};
use cocat_core::room_id::public_id;
use cocat_core::time::Timestamp;
use tokio::task::JoinError;
use yrs::Transact;

use crate::AppState;
use crate::db::{self, DbError, PublicRoom};
use crate::history::diff::replay_until;
use crate::history_api::save_newest_if_changed;
use crate::room::{Author, RoomError};
use crate::ws;

const FETCH_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_REDIRECTS: usize = 5;
/// The other host a provider's host may redirect to: a data repository serving its files
/// from its own storage, as Recherche Data Gouv sends ONERA's to an S3 bucket.
const STORAGE: [(&str, &str); 2] = [
    (
        "entrepot.recherche.data.gouv.fr",
        "prod-onera.opens3r-tls.stockage.inrae.fr",
    ),
    // Each file sits in a bucket of its own: any subdomain of the storage host.
    ("ndownloader.figshare.com", "r2.cloudflarestorage.com"),
];

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
    /// The attributes in the order the provider lists them, the catalogue's columns.
    pub columns: Vec<String>,
    /// The list's events, or why they could not be read this time.
    pub events: Result<Vec<EventRecord>, String>,
}

#[async_trait]
pub trait Provider: Send + Sync {
    /// Lower-case, the middle part of its catalogues' ids: "donki".
    fn id(&self) -> &'static str;
    /// The name people read: "DONKI (NASA CCMC)".
    fn source(&self) -> &'static str;
    /// A cron line, in UTC.
    fn default_schedule(&self) -> &'static str;
    /// The settings the administrator can give, by key; a key holding `key`, `token` or
    /// `secret` is masked on the admin page.
    fn settings(&self) -> &'static [&'static str] {
        &[]
    }
    /// Every list the provider publishes now; the error is what the administrator reads.
    /// `refreshed` says when each list, by key, was last written to its catalogue: a provider
    /// that only fetches what changed since reads it, and fetches a list missing from it (new,
    /// or its catalogue deleted) whole.
    async fn fetch(
        &self,
        http: &reqwest::Client,
        settings: &serde_json::Value,
        refreshed: &HashMap<String, Timestamp>,
    ) -> Result<Vec<Fetched>, String>;
    /// An event the provider no longer lists that stays anyway (it fell outside the window
    /// the provider fetches).
    fn keep(&self, _event: &EventRecord) -> bool {
        false
    }
}

/// The providers this server knows.
pub fn providers() -> Vec<Arc<dyn Provider>> {
    vec![
        Arc::new(donki::Donki),
        Arc::new(cdaw::Cdaw),
        Arc::new(amda::Amda),
        Arc::new(icmecat::Icmecat),
        Arc::new(stix::Stix),
        Arc::new(cactus::Cactus),
        Arc::new(grmb::Grmb),
        Arc::new(helio4cast::Helio4cast),
        Arc::new(pmod::Pmod),
        Arc::new(onera::Onera),
        Arc::new(literature::Literature),
    ]
}

/// What a failed fetch says, down to its cause ("Connection reset by peer"), never with
/// the URL, which may carry a key.
pub fn fetch_error(error: reqwest::Error) -> String {
    let error = error.without_url();
    let mut text = error.to_string();
    let mut cause = std::error::Error::source(&error);
    while let Some(inner) = cause {
        text = format!("{text}: {inner}");
        cause = inner.source();
    }
    text
}

/// Names cocat and its version only: CDAW's firewall resets a connection whose User-Agent
/// holds an address.
pub fn user_agent() -> String {
    format!("cocat-relay/{}", env!("CARGO_PKG_VERSION"))
}

/// The client providers fetch with: a timeout per request, cocat's User-Agent, and redirects
/// followed only on the host asked (or its known storage), so a provider cannot send the
/// server to another one.
/// It speaks TLS through OpenSSL, not rustls: CDAW's firewall resets rustls handshakes
/// (checked 2026-11-10; curl and OpenSSL clients get through).
pub fn http_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .use_native_tls()
        .user_agent(user_agent())
        .timeout(FETCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if follows(attempt.url(), attempt.previous()) {
                attempt.follow()
            } else {
                attempt.error("a redirect to another host, or too many redirects")
            }
        }))
        .build()
}

/// Whether a redirect to `next` is followed, `previous` holding the URLs requested so far,
/// the first being the one asked for.
fn follows(next: &reqwest::Url, previous: &[reqwest::Url]) -> bool {
    let asked = previous.first().and_then(reqwest::Url::host_str);
    let storage = STORAGE.iter().any(|&(repository, storage)| {
        let host = next.host_str().unwrap_or_default();
        let in_storage = host == storage
            || host
                .strip_suffix(storage)
                .is_some_and(|bucket| bucket.ends_with('.'));
        asked == Some(repository) && next.scheme() == "https" && in_storage
    });
    previous.len() <= MAX_REDIRECTS && asked.is_some() && (asked == next.host_str() || storage)
}

/// What a run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    /// How many lists the provider published.
    pub catalogues: usize,
    pub counts: Counts,
    /// How many of its catalogues it no longer publishes.
    pub unpublished: usize,
    /// The lists that could not be read this time, left as they were: "<name>: <why>".
    pub unreadable: Vec<String>,
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

/// Refreshes, then records the outcome whatever it was, a panic or a database error included.
async fn run_to_end(state: AppState, provider: Arc<dyn Provider>) -> Result<RunReport, RelayError> {
    let refreshed = match store::settings(&state.pool, provider.id()).await {
        Ok(settings) => refresh_hiding_secrets(&state, &provider, settings).await,
        Err(error) => Err(error.into()),
    };
    record(&state, provider.id(), &refreshed).await;
    refreshed
}

/// Refreshes on a task of its own, so a panic is caught, with the settings' secrets hidden
/// in what it says.
async fn refresh_hiding_secrets(
    state: &AppState,
    provider: &Arc<dyn Provider>,
    settings: String,
) -> Result<RunReport, RelayError> {
    let secrets = secret_values(&settings);
    tokio::spawn(refresh(state.clone(), provider.clone(), settings))
        .await
        .unwrap_or_else(|error| Err(RelayError::Panicked(panic_message(error))))
        .map(|report| report.without(&secrets))
        .map_err(|error| error.without(&secrets))
}

/// Stores how the run went; when that fails, the provider is still marked stopped.
async fn record(state: &AppState, provider: &str, refreshed: &Result<RunReport, RelayError>) {
    let result = match refreshed {
        Ok(report) => report.as_json(),
        Err(error) => serde_json::json!({ "error": error.to_string() }),
    };
    let ok = refreshed.is_ok();
    let Err(error) = store::finish(&state.pool, provider, ok, &result.to_string()).await else {
        return;
    };
    tracing::error!(%error, provider, "the end of a relay run was not recorded");
    if let Err(error) = store::stop(&state.pool, provider).await {
        tracing::error!(%error, provider, "a relay run stays marked running until a restart");
    }
}

/// The non-empty values of the settings the admin page masks.
fn secret_values(settings: &str) -> Vec<String> {
    match serde_json::from_str(settings) {
        Ok(serde_json::Value::Object(settings)) => settings
            .into_iter()
            .filter(|(key, _)| admin::is_secret(key))
            .filter_map(|(_, value)| value.as_str().map(str::to_owned))
            .filter(|value| !value.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

impl RelayError {
    /// The error with every secret in what the provider said replaced, so that neither the
    /// stored result nor a log line carries one.
    fn without(self, secrets: &[String]) -> Self {
        match self {
            Self::Fetch(text) => Self::Fetch(hide(text, secrets)),
            Self::Panicked(text) => Self::Panicked(hide(text, secrets)),
            other => other,
        }
    }
}

fn hide(text: String, secrets: &[String]) -> String {
    secrets
        .iter()
        .fold(text, |text, secret| text.replace(secret, "•••"))
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

async fn refresh(
    state: AppState,
    provider: Arc<dyn Provider>,
    settings: String,
) -> Result<RunReport, RelayError> {
    let settings = serde_json::from_str(&settings)?;
    let refreshed = refreshed_lists(&state, provider.id()).await?;
    let fetched = provider
        .fetch(&state.http, &settings, &refreshed)
        .await
        .map_err(RelayError::Fetch)?;
    if fetched.is_empty() {
        return Err(RelayError::Empty);
    }
    let catalogues = fetched.len();
    let mut rooms = Vec::new();
    let mut counts = Counts::default();
    let mut unreadable = Vec::new();
    // simplify: every list is held at once, as `fetch` returns them all (AMDA: ~300k events,
    // ~150 MB at peak); write list by list as fetched if that grows.
    for mut list in fetched {
        let room = match public_id(&format!("{}.{}", provider.id(), list.key)) {
            Ok(room) => room,
            Err(error) => {
                unreadable.push(format!("{}: {error}", list.name));
                continue;
            }
        };
        match std::mem::replace(&mut list.events, Ok(Vec::new())) {
            Ok(events) => match relay_list(&state, &provider, &room, &list, events).await? {
                Some(written) => counts += written,
                None => unreadable.push(format!(
                    "{}: {} sent no events; the catalogue keeps those it holds",
                    list.name,
                    provider.source()
                )),
            },
            Err(error) => unreadable.push(format!("{}: {error}", list.name)),
        }
        rooms.push(room);
    }
    let unpublished = store::unpublish_others(&state.pool, provider.id(), &rooms).await?;
    Ok(RunReport {
        catalogues,
        counts,
        unpublished,
        unreadable,
    })
}

/// When each of the provider's catalogues was last refreshed, by list key.
async fn refreshed_lists(
    state: &AppState,
    provider: &str,
) -> Result<HashMap<String, Timestamp>, RelayError> {
    let rows = store::refreshed_lists(&state.pool, provider).await?;
    Ok(rows
        .into_iter()
        .filter_map(|(key, at)| Some((key, Timestamp::parse(&at)?)))
        .collect())
}

/// The catalogue's columns: those the provider names, else every attribute its events carry,
/// in the order they first appear, so the table shows a list's measures.
fn columns_of(named: &[String], events: &[EventRecord]) -> Vec<String> {
    if !named.is_empty() {
        return named.to_vec();
    }
    let mut seen = Vec::new();
    for key in events.iter().flat_map(|event| event.attributes.keys()) {
        if !seen.contains(key) {
            seen.push(key.clone());
        }
    }
    seen
}

/// Makes public catalogue `room` the provider's list: what the index says, then its events,
/// as one version named after the source when something changed. `None` when the list is
/// empty and the catalogue holds events it would remove: an empty answer is more likely a
/// glitch at the source than a list emptied, so the catalogue is left as it is.
async fn relay_list(
    state: &AppState,
    provider: &Arc<dyn Provider>,
    room: &str,
    list: &Fetched,
    events: Vec<EventRecord>,
) -> Result<Option<Counts>, RelayError> {
    place(state, provider.as_ref(), room, list).await?;
    let empty = events.is_empty();
    let wanted = Wanted {
        catalogue: list.catalogue.clone(),
        name: list.name.clone(),
        columns: columns_of(&list.columns, &events),
        events,
    };
    let (counts, update) = held(state, provider.clone(), room, wanted).await?;
    if empty && counts.removed > 0 {
        return Ok(None);
    }
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
    Ok(Some(counts))
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
    /// The report with every secret in why a list could not be read replaced.
    fn without(self, secrets: &[String]) -> Self {
        let unreadable = self
            .unreadable
            .into_iter()
            .map(|text| hide(text, secrets))
            .collect();
        Self { unreadable, ..self }
    }

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
            "unreadable": self.unreadable,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_without_named_columns_shows_its_attributes_in_first_seen_order() {
        let mut first = EventRecord::new(
            "a",
            Timestamp::from_unix_nanos(0).unwrap(),
            Timestamp::from_unix_nanos(1).unwrap(),
        );
        first
            .attributes
            .insert("speed_km_s".into(), serde_json::json!(400));
        first
            .attributes
            .insert("width_deg".into(), serde_json::json!(90));
        let mut second = first.clone();
        second
            .attributes
            .insert("remarks".into(), serde_json::json!("Halo"));
        let events = [first, second];
        assert_eq!(
            columns_of(&[], &events),
            ["speed_km_s", "width_deg", "remarks"]
        );
        let named = ["width_deg".to_owned()];
        assert_eq!(columns_of(&named, &events), ["width_deg"]);
        assert!(columns_of(&[], &[]).is_empty());
    }

    #[test]
    fn the_user_agent_names_cocat_and_no_address() {
        let agent = user_agent();
        assert!(agent.starts_with("cocat-relay/"), "{agent}");
        assert!(
            !agent.contains("http"),
            "CDAW refuses an address in it: {agent}"
        );
    }

    #[test]
    fn why_a_list_could_not_be_read_carries_no_secret() {
        let report = RunReport {
            catalogues: 1,
            counts: Counts::default(),
            unpublished: 0,
            unreadable: vec!["flares: 403 for api_key=s3cr3t".to_owned()],
        };

        let report = report.without(&["s3cr3t".to_owned()]);

        assert_eq!(report.unreadable, ["flares: 403 for api_key=•••"]);
    }

    fn url(text: &str) -> reqwest::Url {
        reqwest::Url::parse(text).unwrap()
    }

    #[test]
    fn a_redirect_is_followed_only_on_the_same_host_and_at_most_five_times() {
        let asked = url("https://cdaw.gsfc.nasa.gov/CME_list/a.txt");
        let same = url("https://cdaw.gsfc.nasa.gov/CME_list/b.txt");

        assert!(follows(&same, std::slice::from_ref(&asked)));
        assert!(!follows(
            &url("https://evil.example.org/a.txt"),
            std::slice::from_ref(&asked)
        ));
        assert!(!follows(
            &url("http://localhost:8080/admin"),
            std::slice::from_ref(&asked)
        ));
        assert!(follows(&same, &vec![asked.clone(); 5]));
        assert!(!follows(&same, &vec![asked; 6]));
    }

    #[test]
    fn a_repository_redirect_is_followed_only_to_its_own_storage() {
        let asked = url("https://entrepot.recherche.data.gouv.fr/api/access/datafile/1");
        let storage = url("https://prod-onera.opens3r-tls.stockage.inrae.fr/10.57745/X/abc");

        assert!(follows(&storage, std::slice::from_ref(&asked)));
        assert!(!follows(
            &url("http://prod-onera.opens3r-tls.stockage.inrae.fr/10.57745/X/abc"),
            std::slice::from_ref(&asked)
        ));
        assert!(!follows(
            &storage,
            &[url("https://cdaw.gsfc.nasa.gov/CME_list/a.txt")]
        ));
    }

    #[test]
    fn figshare_hands_its_downloads_to_any_bucket_of_its_r2_storage() {
        let asked = url("https://ndownloader.figshare.com/files/7428409");
        let bucket = url(
            "https://44c1cec316ec56bca76dfd65d2882b20.r2.cloudflarestorage.com/figshare-com-storage/7428409/a.txt",
        );

        assert!(follows(&bucket, std::slice::from_ref(&asked)));
        assert!(!follows(
            &url("https://r2.cloudflarestorage.com.evil.example/a.txt"),
            std::slice::from_ref(&asked)
        ));
        assert!(!follows(
            &bucket,
            &[url("https://zenodo.org/api/records/1/files/a/content")]
        ));
    }
}
