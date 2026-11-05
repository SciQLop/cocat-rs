//! Outgoing mail, configured from the environment.
//!
//! Without `COCAT_SMTP_HOST` or `COCAT_MAIL_DIR` there is no sender at all: the server still
//! runs, no verification is asked for, and the API hands out the links it cannot mail.

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use email_address::EmailAddress;
use lettre::message::Mailbox;
use lettre::transport::file::AsyncFileTransport;
use lettre::transport::smtp::AsyncSmtpTransport;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncTransport, Message, Tokio1Executor};
use serde::Deserialize;
use thiserror::Error;

const PLACEHOLDER_DOMAIN: &str = "placeholder.cocat.org";

const SMTP_TIMEOUT: Duration = Duration::from_secs(10);

const DEFAULT_PORT: u16 = 587;

const TRUTHY: [&str; 4] = ["1", "true", "yes", "on"];

const FALSEY: [&str; 4] = ["0", "false", "no", "off"];

#[derive(Debug, Clone)]
pub enum Mailer {
    Smtp(SmtpMailer),
    Directory(DirectoryMailer),
}

/// A submission server.
///
/// Implicit TLS on port 465 is not supported: plain submission with STARTTLS covers the
/// relays the README documents.
#[derive(Debug, Clone)]
pub struct SmtpMailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    sender: Mailbox,
    host: String,
    starttls: bool,
}

/// Writes each message as an `.eml` file: the test sink and a local development aid.
#[derive(Debug, Clone)]
pub struct DirectoryMailer {
    transport: AsyncFileTransport<Tokio1Executor>,
    sender: Mailbox,
    path: PathBuf,
}

#[derive(Debug, Error)]
pub enum MailError {
    #[error("COCAT_MAIL_FROM is required when a mailer is configured")]
    MissingSender,
    #[error("COCAT_MAIL_FROM must be a mail address, not {0:?}")]
    SenderAddress(String),
    #[error("COCAT_SMTP_PORT must be a number, not {0:?}")]
    InvalidPort(String),
    #[error("COCAT_SMTP_STARTTLS must be one of 1, true, yes, on, 0, false, no, off, not {0:?}")]
    InvalidStarttls(String),
    #[error("COCAT_SMTP_PASSWORD is required with COCAT_SMTP_USERNAME")]
    MissingPassword,
    #[error("COCAT_SMTP_USERNAME is required with COCAT_SMTP_PASSWORD")]
    MissingUsername,
    #[error("COCAT_MAIL_DIR and COCAT_SMTP_HOST cannot both be set")]
    TwoSinks,
    #[error(
        "COCAT_PUBLIC_URL is required when a mailer is configured: a mailed link must not be built from the request's Host header"
    )]
    MissingPublicUrl,
    #[error("{0:?} is not a mail address")]
    Address(String),
    #[error("a mail header cannot contain a line break")]
    LineBreak,
    #[error("{0} is a placeholder address and receives no mail")]
    Placeholder(String),
    #[error("the message could not be built: {0}")]
    Build(#[from] lettre::error::Error),
    #[error("the SMTP transport could not be configured: {0}")]
    Configuration(#[from] lettre::transport::smtp::Error),
    #[error("{host} refused the message: {source}")]
    Relay {
        host: String,
        #[source]
        source: lettre::transport::smtp::Error,
    },
    #[error("{path} could not take the message: {source}")]
    Sink {
        path: String,
        #[source]
        source: lettre::transport::file::Error,
    },
}

impl Mailer {
    /// Reads the sender from the environment, or `None` when none is configured.
    ///
    /// A configured sender needs `COCAT_PUBLIC_URL`: the only thing it can send is a link,
    /// and a link built from the request's `Host` header would hand the token to whoever
    /// controls that header.
    pub fn from_env(env: &impl Fn(&str) -> Option<String>) -> Result<Option<Mailer>, MailError> {
        let host = setting(env, "COCAT_SMTP_HOST");
        let directory = setting(env, "COCAT_MAIL_DIR");
        if host.is_empty() && directory.is_empty() {
            return Ok(None);
        }
        if !host.is_empty() && !directory.is_empty() {
            return Err(MailError::TwoSinks);
        }
        let sender = sender(env)?;
        let mailer = if host.is_empty() {
            let path = PathBuf::from(directory);
            Mailer::Directory(DirectoryMailer {
                transport: AsyncFileTransport::new(path.clone()),
                sender,
                path,
            })
        } else {
            Mailer::Smtp(smtp(env, host, sender)?)
        };
        if setting(env, "COCAT_PUBLIC_URL").is_empty() {
            return Err(MailError::MissingPublicUrl);
        }
        Ok(Some(mailer))
    }

    pub async fn send(&self, to: &str, subject: &str, body: &str) -> Result<(), MailError> {
        let message = self.message(to, subject, body)?;
        match self {
            Mailer::Smtp(smtp) => {
                smtp.transport
                    .send(message)
                    .await
                    .map(|_| ())
                    .map_err(|source| MailError::Relay {
                        host: smtp.host.clone(),
                        source,
                    })
            }
            Mailer::Directory(directory) => directory
                .transport
                .send(message)
                .await
                .map(|_| ())
                .map_err(|source| MailError::Sink {
                    path: directory.path.display().to_string(),
                    source,
                }),
        }
    }

    fn message(&self, to: &str, subject: &str, body: &str) -> Result<Message, MailError> {
        let recipient = recipient(to)?;
        if subject.contains(['\r', '\n']) {
            return Err(MailError::LineBreak);
        }
        Message::builder()
            .from(self.sender().clone())
            .to(recipient)
            .subject(subject)
            .body(body.to_string())
            .map_err(MailError::Build)
    }

    fn sender(&self) -> &Mailbox {
        match self {
            Mailer::Smtp(smtp) => &smtp.sender,
            Mailer::Directory(directory) => &directory.sender,
        }
    }
}

impl SmtpMailer {
    pub fn starttls(&self) -> bool {
        self.starttls
    }
}

/// Whether an address is the stand-in kept for accounts a provider gave no address for.
///
/// `placeholder.cocat.org` is reserved for that stand-in, one subdomain per provider, and
/// nothing is ever mailed to it.
pub fn is_placeholder(email: &str) -> bool {
    let domain = match email.rsplit_once('@') {
        Some((_, domain)) => domain.to_lowercase(),
        None => String::new(),
    };
    domain == PLACEHOLDER_DOMAIN || domain.ends_with(&format!(".{PLACEHOLDER_DOMAIN}"))
}

/// An address as Python's schema accepts it: trimmed, syntactically valid, and with a period
/// in the domain, which is the one rule `email-validator` adds to the grammar.
#[derive(Debug, Deserialize)]
#[serde(try_from = "String")]
pub struct Address(pub String);

impl TryFrom<String> for Address {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let trimmed = value.trim();
        let address = EmailAddress::from_str(trimmed).map_err(|_| NOT_AN_ADDRESS)?;
        if !address.domain().contains('.') {
            return Err(NOT_AN_ADDRESS);
        }
        Ok(Self(trimmed.to_owned()))
    }
}

const NOT_AN_ADDRESS: &str = "the email must be an email address";

pub fn verification_mail(link: &str) -> (&'static str, String) {
    (
        "Confirm your cocat address",
        format!(
            "Open this link to confirm your address:\n\n{link}\n\n\
             The link is valid for an hour. If you did not sign up, ignore this message.\n"
        ),
    )
}

pub fn reset_mail(link: &str) -> (&'static str, String) {
    (
        "Reset your cocat password",
        format!(
            "Open this link to choose a new password:\n\n{link}\n\n\
             The link is valid for an hour. If you did not ask for it, ignore this message.\n"
        ),
    )
}

pub fn invitation_mail(room_name: &str, inviter: &str, link: &str) -> (String, String) {
    (
        format!("{inviter} invites you to the room {room_name}"),
        format!(
            "{inviter} invites you to join the room \"{room_name}\":\n\n{link}\n\n\
             The invitation is valid for 14 days.\n"
        ),
    )
}

pub fn group_invitation_mail(group_name: &str, inviter: &str, link: &str) -> (String, String) {
    (
        format!("{inviter} invites you to the group {group_name}"),
        format!(
            "{inviter} invites you to join the group \"{group_name}\":\n\n{link}\n\n\
             The invitation is valid for 14 days.\n"
        ),
    )
}

fn setting(env: &impl Fn(&str) -> Option<String>, name: &str) -> String {
    env(name).unwrap_or_default().trim().to_string()
}

fn sender(env: &impl Fn(&str) -> Option<String>) -> Result<Mailbox, MailError> {
    let value = setting(env, "COCAT_MAIL_FROM");
    if value.is_empty() {
        return Err(MailError::MissingSender);
    }
    if value.contains(['\r', '\n']) {
        return Err(MailError::LineBreak);
    }
    Mailbox::from_str(&value).map_err(|_| MailError::SenderAddress(value))
}

fn recipient(to: &str) -> Result<Mailbox, MailError> {
    if to.contains(['\r', '\n']) {
        return Err(MailError::LineBreak);
    }
    if is_placeholder(to) {
        return Err(MailError::Placeholder(to.to_string()));
    }
    Mailbox::from_str(to).map_err(|_| MailError::Address(to.to_string()))
}

fn smtp(
    env: &impl Fn(&str) -> Option<String>,
    host: String,
    sender: Mailbox,
) -> Result<SmtpMailer, MailError> {
    let port = port(env)?;
    let starttls = starttls(env)?;
    let (username, password) = credentials(env)?;
    let mut builder = if starttls {
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&host)?
    } else {
        AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&host)
    }
    .port(port)
    .timeout(Some(SMTP_TIMEOUT));
    if let (Some(username), Some(password)) = (username, password) {
        builder = builder.credentials(Credentials::new(username, password));
    }
    Ok(SmtpMailer {
        transport: builder.build(),
        sender,
        host,
        starttls,
    })
}

fn port(env: &impl Fn(&str) -> Option<String>) -> Result<u16, MailError> {
    let value = setting(env, "COCAT_SMTP_PORT");
    if value.is_empty() {
        return Ok(DEFAULT_PORT);
    }
    value
        .parse()
        .map_err(|_| MailError::InvalidPort(value.clone()))
}

fn starttls(env: &impl Fn(&str) -> Option<String>) -> Result<bool, MailError> {
    let value = setting(env, "COCAT_SMTP_STARTTLS");
    if value.is_empty() {
        return Ok(true);
    }
    let word = value.to_lowercase();
    if TRUTHY.contains(&word.as_str()) {
        return Ok(true);
    }
    if FALSEY.contains(&word.as_str()) {
        return Ok(false);
    }
    Err(MailError::InvalidStarttls(value))
}

fn credentials(
    env: &impl Fn(&str) -> Option<String>,
) -> Result<(Option<String>, Option<String>), MailError> {
    let username = env("COCAT_SMTP_USERNAME").filter(|value| !value.is_empty());
    let password = env("COCAT_SMTP_PASSWORD").filter(|value| !value.is_empty());
    match (username, password) {
        (Some(_), None) => Err(MailError::MissingPassword),
        (None, Some(_)) => Err(MailError::MissingUsername),
        pair => Ok(pair),
    }
}
