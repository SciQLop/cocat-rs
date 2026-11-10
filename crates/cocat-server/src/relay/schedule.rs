//! When providers run: each wakes on its cron line (UTC), and a provider that missed its
//! slot while the server was down runs once at start.

use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use croner::Cron;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::task::{JoinHandle, JoinSet};

use super::{Provider, run, store};
use crate::AppState;

const WAKE_EVERY: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    #[error("not a valid cron line: {0}")]
    Invalid(String),
    #[error("the cron line has no next time")]
    OutOfRange,
}

/// Which past run a schedule counts from: the last that worked at start, so a failed run
/// is made up for; the last attempt while running, so a failing provider is not hammered.
#[derive(Debug, Clone, Copy)]
pub enum Since {
    Success,
    Attempt,
}

pub fn parse(schedule: &str) -> Result<Cron, ScheduleError> {
    Cron::from_str(schedule).map_err(|error| ScheduleError::Invalid(error.to_string()))
}

/// When the provider is next due: the first slot after `last_ok`, or now when it never ran.
pub fn next_due(
    schedule: &str,
    last_ok: Option<OffsetDateTime>,
    now: OffsetDateTime,
) -> Result<OffsetDateTime, ScheduleError> {
    let cron = parse(schedule)?;
    let Some(last) = last_ok else { return Ok(now) };
    let last = DateTime::<Utc>::from_timestamp(last.unix_timestamp(), 0)
        .ok_or(ScheduleError::OutOfRange)?;
    let next = cron
        .find_next_occurrence(&last, false)
        .map_err(|_| ScheduleError::OutOfRange)?;
    OffsetDateTime::from_unix_timestamp(next.timestamp()).map_err(|_| ScheduleError::OutOfRange)
}

/// Runs every enabled provider that is due at `now`, the providers side by side, and waits
/// for them. A run is skipped, not queued, while the provider is still running.
pub async fn run_due(state: &AppState, now: OffsetDateTime, since: Since) {
    let mut runs = JoinSet::new();
    for provider in state.providers.iter() {
        if is_due(state, provider.as_ref(), now, since).await {
            let (state, id) = (state.clone(), provider.id());
            runs.spawn(async move {
                if let Err(error) = run(&state, id).await {
                    tracing::warn!(%error, provider = id, "a scheduled relay run did not work");
                }
            });
        }
    }
    while runs.join_next().await.is_some() {}
}

async fn is_due(
    state: &AppState,
    provider: &dyn Provider,
    now: OffsetDateTime,
    since: Since,
) -> bool {
    let row = match store::provider(&state.pool, provider.id()).await {
        Ok(Some(row)) if row.enabled && !row.running => row,
        Ok(_) => return false,
        Err(error) => {
            tracing::warn!(%error, provider = provider.id(), "the relay schedule cannot be read");
            return false;
        }
    };
    let schedule = row
        .schedule
        .as_deref()
        .unwrap_or(provider.default_schedule());
    let last = match since {
        Since::Success => row.last_ok_at,
        Since::Attempt => row.last_run_at,
    }
    .and_then(|stamp| OffsetDateTime::parse(&stamp, &Rfc3339).ok());
    match next_due(schedule, last, now) {
        Ok(due) => due <= now,
        Err(error) => {
            tracing::warn!(%error, provider = provider.id(), "the relay schedule is unusable");
            false
        }
    }
}

/// Wakes every minute; the first pass, at once, is the start-up one.
pub fn spawn_scheduler(state: AppState) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(WAKE_EVERY);
        let mut since = Since::Success;
        loop {
            ticker.tick().await;
            let state = state.clone();
            tokio::spawn(async move { run_due(&state, OffsetDateTime::now_utc(), since).await });
            since = Since::Attempt;
        }
    })
}
