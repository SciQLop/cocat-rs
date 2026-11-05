//! The cocat server: room websockets, authentication and the seeding CLI.

pub mod auth;
pub mod cli;
pub mod db;
pub mod lifecycle;
pub mod room;
pub mod ws;

use std::sync::Arc;

use futures_util::future::BoxFuture;
use sqlx::SqlitePool;

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
}
