//! What the administrator does with providers: see them, configure them, refresh one now.
//! A caller who is not a superuser gets `NotFound`, as everywhere in administration.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{Map, Value};

use super::{Provider, run, schedule, store};
use crate::AppState;
use crate::admin::{AdminError, require};
use crate::db::User;

const SECRET_MARKS: [&str; 3] = ["key", "token", "secret"];

/// A provider and its stored row, if the administrator ever touched it.
pub struct Status {
    pub provider: Arc<dyn Provider>,
    pub row: Option<store::ProviderRow>,
}

/// What the administrator submitted for one provider.
pub struct Config {
    pub enabled: bool,
    pub schedule: String,
    /// The form's `setting.<key>` fields, as `(key, value)`.
    pub settings: Vec<(String, String)>,
}

/// One setting as the form shows it.
#[derive(Debug, PartialEq, Eq)]
pub struct Setting {
    pub key: String,
    pub secret: bool,
    /// What is stored; never filled for a secret.
    pub value: String,
    pub is_set: bool,
}

pub fn is_secret(key: &str) -> bool {
    let key = key.to_lowercase();
    SECRET_MARKS.iter().any(|mark| key.contains(mark))
}

impl Status {
    pub fn schedule(&self) -> &str {
        self.row
            .as_ref()
            .and_then(|row| row.schedule.as_deref())
            .unwrap_or(self.provider.default_schedule())
    }

    /// The settings the provider declares plus any already stored, by key.
    pub fn settings(&self) -> Vec<Setting> {
        let stored = self
            .row
            .as_ref()
            .map_or_else(Map::new, |row| stored_settings(&row.settings));
        let keys: BTreeSet<&str> = self
            .provider
            .settings()
            .iter()
            .copied()
            .chain(stored.keys().map(String::as_str))
            .collect();
        keys.into_iter()
            .map(|key| {
                let value = stored.get(key).and_then(Value::as_str).unwrap_or_default();
                let secret = is_secret(key);
                Setting {
                    key: key.to_owned(),
                    secret,
                    value: if secret { "" } else { value }.to_owned(),
                    is_set: !value.is_empty(),
                }
            })
            .collect()
    }
}

fn stored_settings(text: &str) -> Map<String, Value> {
    match serde_json::from_str(text) {
        Ok(Value::Object(settings)) => settings,
        _ => Map::new(),
    }
}

pub async fn statuses(state: &AppState, actor: &User) -> Result<Vec<Status>, AdminError> {
    require(actor)?;
    let mut statuses = Vec::new();
    for provider in state.providers.iter() {
        let row = store::provider(&state.pool, provider.id()).await?;
        statuses.push(Status {
            provider: provider.clone(),
            row,
        });
    }
    Ok(statuses)
}

fn find(state: &AppState, id: &str) -> Result<Arc<dyn Provider>, AdminError> {
    state
        .providers
        .iter()
        .find(|provider| provider.id() == id)
        .cloned()
        .ok_or(AdminError::UnknownProvider)
}

/// Saves the form, or saves nothing when the cron line is not valid.
pub async fn configure(
    state: &AppState,
    actor: &User,
    id: &str,
    config: Config,
) -> Result<(), AdminError> {
    require(actor)?;
    let status = Status {
        provider: find(state, id)?,
        row: store::provider(&state.pool, id).await?,
    };
    let schedule = Some(config.schedule.trim()).filter(|line| !line.is_empty());
    if let Some(line) = schedule {
        schedule::parse(line)?;
    }
    let settings = merge_settings(&status, &config.settings);
    store::configure(
        &state.pool,
        id,
        config.enabled,
        schedule,
        &Value::Object(settings).to_string(),
    )
    .await?;
    Ok(())
}

/// The stored settings with the submitted ones applied. Only known keys are taken. An empty
/// secret keeps its old value, an empty plain setting is cleared.
fn merge_settings(status: &Status, submitted: &[(String, String)]) -> Map<String, Value> {
    let mut merged = status
        .row
        .as_ref()
        .map_or_else(Map::new, |row| stored_settings(&row.settings));
    for setting in status.settings() {
        let Some((_, value)) = submitted.iter().find(|(key, _)| *key == setting.key) else {
            continue;
        };
        match (value.trim(), setting.secret) {
            ("", true) => {}
            ("", false) => {
                merged.remove(&setting.key);
            }
            (value, _) => {
                merged.insert(setting.key, Value::from(value));
            }
        }
    }
    merged
}

/// What `refresh_now` did.
pub enum Refresh {
    Started(&'static str),
    AlreadyRunning(&'static str),
}

/// Starts a run in the background; the page shows how it went when it is reloaded.
pub async fn refresh_now(state: &AppState, actor: &User, id: &str) -> Result<Refresh, AdminError> {
    require(actor)?;
    let provider = find(state, id)?;
    let source = provider.source();
    let running = store::provider(&state.pool, id)
        .await?
        .is_some_and(|row| row.running);
    if running {
        return Ok(Refresh::AlreadyRunning(source));
    }
    let state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = run(&state, provider.id()).await {
            tracing::warn!(%error, provider = provider.id(), "a requested relay run did not work");
        }
    });
    Ok(Refresh::Started(source))
}
