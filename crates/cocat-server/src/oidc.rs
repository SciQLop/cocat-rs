//! OpenID Connect providers and the account rules a provider sign-in follows.

use openidconnect::core::{CoreClient, CoreProviderMetadata};
use openidconnect::{
    ClientId, ClientSecret, DiscoveryError, EndpointMaybeSet, EndpointNotSet, EndpointSet,
    IssuerUrl, RedirectUrl,
};
use serde::Deserialize;
use sqlx::SqlitePool;
use thiserror::Error;
use uuid::Uuid;

use crate::links::Links;
use crate::{AppState, db, mail};

/// The client a successful discovery yields: metadata names its endpoints, which the configuration
/// may still leave out.
type Discovered = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

const NAME_PATTERN: &str = "^[a-z0-9_-]{1,32}$";

/// One entry of `COCAT_OAUTH_PROVIDERS`, in Python's shape.
#[derive(Clone, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub discovery_url: String,
    pub client_id: String,
    pub client_secret: String,
    #[serde(default)]
    pub verifies_email: bool,
    #[serde(default = "default_scopes")]
    pub scopes: Vec<String>,
}

fn default_scopes() -> Vec<String> {
    vec!["openid".to_owned(), "email".to_owned()]
}

/// The secret must not reach a log line, so it is redacted rather than derived.
impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderConfig")
            .field("name", &self.name)
            .field("discovery_url", &self.discovery_url)
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("verifies_email", &self.verifies_email)
            .field("scopes", &self.scopes)
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum OidcError {
    #[error("COCAT_OAUTH_PROVIDERS is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("COCAT_OAUTH_PROVIDERS must be a JSON list of providers")]
    NotAList,
    #[error("provider name {0:?} must match {NAME_PATTERN}")]
    InvalidName(String),
    #[error(
        "OpenID Connect providers need COCAT_PUBLIC_URL so that their redirect URI is absolute"
    )]
    MissingPublicUrl,
}

/// A provider that answered discovery, ready to sign someone in.
#[derive(Clone)]
pub struct Provider {
    pub config: ProviderConfig,
    pub client: Discovered,
}

/// What the provider says about the visitor once the code is exchanged.
#[derive(Debug)]
pub struct Identity {
    pub subject: String,
    pub email: Option<String>,
}

#[derive(Debug, Error)]
pub enum SignInError {
    #[error("that address already has an account")]
    Exists,
    #[error(transparent)]
    Db(#[from] db::DbError),
}

pub fn providers_from_env(
    env: &impl Fn(&str) -> Option<String>,
) -> Result<Vec<ProviderConfig>, OidcError> {
    let configured = env("COCAT_OAUTH_PROVIDERS").unwrap_or_default();
    if configured.trim().is_empty() {
        return Ok(Vec::new());
    }
    let value: serde_json::Value = serde_json::from_str(&configured)?;
    let Some(entries) = value.as_array() else {
        return Err(OidcError::NotAList);
    };
    let configs = entries
        .iter()
        .cloned()
        .map(serde_json::from_value)
        .collect::<Result<Vec<ProviderConfig>, _>>()?;
    for config in &configs {
        if !valid_name(&config.name) {
            return Err(OidcError::InvalidName(config.name.clone()));
        }
    }
    let public_url = env("COCAT_PUBLIC_URL").unwrap_or_default();
    if !configs.is_empty() && public_url.trim().is_empty() {
        return Err(OidcError::MissingPublicUrl);
    }
    Ok(configs)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '_'
                || character == '-'
        })
}

/// The client sign-in talks to identity providers with. It asks for rustls: the relay
/// enables reqwest's `native-tls` for its own client, which would make OpenSSL every client's
/// default. Redirects are not followed, as OpenID Connect requires.
pub fn http_client() -> Result<openidconnect::reqwest::Client, openidconnect::reqwest::Error> {
    openidconnect::reqwest::ClientBuilder::new()
        .use_rustls_tls()
        .redirect(openidconnect::reqwest::redirect::Policy::none())
        .build()
}

/// Discover every provider, keeping those that answer; one that is down only loses its login.
pub async fn discover(configs: Vec<ProviderConfig>, links: &Links) -> Vec<Provider> {
    let http_client = match http_client() {
        Ok(client) => client,
        Err(error) => {
            tracing::warn!(%error, "cannot build the OpenID Connect client");
            return Vec::new();
        }
    };
    let mut providers = Vec::with_capacity(configs.len());
    for config in configs {
        let name = config.name.clone();
        match connected(config, links, &http_client).await {
            Ok(provider) => providers.push(provider),
            Err(error) => {
                tracing::warn!(%error, provider = %name, "OpenID Connect provider is unreachable; its login is off");
            }
        }
    }
    providers
}

#[derive(Debug, Error)]
enum ConnectError {
    #[error(transparent)]
    Url(#[from] openidconnect::url::ParseError),
    #[error(transparent)]
    Discovery(
        #[from] DiscoveryError<openidconnect::HttpClientError<openidconnect::reqwest::Error>>,
    ),
}

/// The configuration names the discovery document, which is what Python's files carry; the
/// crate wants the issuer and appends the same well-known path itself.
fn issuer(config: &ProviderConfig) -> &str {
    config
        .discovery_url
        .strip_suffix("/.well-known/openid-configuration")
        .unwrap_or(&config.discovery_url)
}

async fn connected(
    config: ProviderConfig,
    links: &Links,
    http_client: &openidconnect::reqwest::Client,
) -> Result<Provider, ConnectError> {
    let metadata = CoreProviderMetadata::discover_async(
        IssuerUrl::new(issuer(&config).to_owned())?,
        http_client,
    )
    .await?;
    let client = CoreClient::from_provider_metadata(
        metadata,
        ClientId::new(config.client_id.clone()),
        Some(ClientSecret::new(config.client_secret.clone())),
    )
    .set_redirect_uri(RedirectUrl::new(
        links.ui(&format!("/oauth/{}/callback", config.name)),
    )?);
    Ok(Provider { config, client })
}

/// Sign the visitor in, creating or linking the account Python's rules name.
pub async fn sign_in(
    state: &AppState,
    provider: &ProviderConfig,
    identity: Identity,
) -> Result<db::User, SignInError> {
    let (email, trusted) = account_email(provider, &identity);
    let mut transaction = db::begin_immediate(&state.pool).await?;
    if let Some(user) = db::oauth_user(&mut *transaction, &provider.name, &identity.subject).await?
    {
        transaction.commit().await.map_err(db::DbError::from)?;
        return Ok(user);
    }
    let associates = trusted && state.mailer.is_some();
    if let Some(existing) = db::user_by_email(&mut *transaction, &email).await? {
        if !(associates && existing.is_verified) {
            return Err(SignInError::Exists);
        }
        db::link_oauth_account(
            &mut *transaction,
            &provider.name,
            &identity.subject,
            existing.id,
        )
        .await?;
        transaction.commit().await.map_err(db::DbError::from)?;
        return Ok(existing);
    }
    let id = Uuid::new_v4();
    // Nobody is ever told this password: the provider is the only way into the account.
    let password = Uuid::new_v4().to_string();
    let hashed_password = db::hash_password(&password).await?;
    db::insert_user(
        &mut *transaction,
        id,
        &email,
        &hashed_password,
        false,
        trusted,
    )
    .await?;
    db::link_oauth_account(&mut *transaction, &provider.name, &identity.subject, id).await?;
    transaction.commit().await.map_err(db::DbError::from)?;
    let user = db::user_by_id(&state.pool, id)
        .await?
        .ok_or_else(|| db::DbError::Sqlx(sqlx::Error::RowNotFound))?;
    db::create_personal_room(&state.pool, id).await?;
    Ok(user)
}

/// The address the account gets, and whether it came from a provider this deployment trusts.
fn account_email(provider: &ProviderConfig, identity: &Identity) -> (String, bool) {
    let claimed = identity
        .email
        .as_deref()
        .filter(|_| provider.verifies_email)
        .and_then(|email| mail::Address::try_from(email.to_owned()).ok())
        .filter(|address| !mail::is_placeholder(&address.0));
    match claimed {
        Some(address) => (address.0, true),
        None => (
            mail::placeholder_address(&provider.name, &identity.subject),
            false,
        ),
    }
}

pub async fn linked_providers(pool: &SqlitePool, user: Uuid) -> Result<Vec<String>, db::DbError> {
    db::linked_providers(pool, user).await
}
