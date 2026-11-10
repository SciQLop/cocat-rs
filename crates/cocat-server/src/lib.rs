//! The cocat server: room websockets, authentication and the seeding CLI.

pub mod accounts;
pub mod accounts_api;
pub mod admin;
pub mod api;
pub mod auth;
pub mod catalogues_api;
pub mod cli;
pub mod db;
pub mod documents;
pub mod groups;
pub mod groups_api;
pub mod hashing;
pub mod history;
pub mod history_api;
pub mod home_api;
pub mod import;
pub mod invitations;
pub mod invitations_api;
pub mod lifecycle;
pub mod links;
pub mod mail;
pub mod oidc;
pub mod openapi;
pub mod password;
pub mod presence;
pub mod public_api;
pub mod ratelimit;
pub mod room;
pub mod sign_in;
pub mod sign_in_api;
pub mod submissions;
pub mod submissions_api;
pub mod telemetry;
pub mod tokens_api;
pub mod ui;
pub mod ws;

use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use sqlx::SqlitePool;

use crate::links::Links;
use crate::mail::Mailer;
use crate::oidc::Provider;
use crate::ratelimit::LoginLimit;
use crate::room::RoomManager;

/// Runs once per socket, after the handshake granted a role and before the socket attaches.
/// `serve` leaves it unset; a test uses it to change a role in exactly that window.
pub type BeforeAttach = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub secret: String,
    pub rooms: RoomManager,
    pub before_attach: Option<BeforeAttach>,
    pub login_limit: LoginLimit,
    pub mailer: Option<Mailer>,
    pub links: Links,
    /// The providers that answered discovery at startup; empty keeps the password login alone.
    pub oidc: Vec<Provider>,
    /// `COCAT_WEB_DIR`: the catalogue browser's bundle; `None` answers that it is not installed.
    pub web_dir: Option<PathBuf>,
}
