//! The cocat server: room websockets, authentication and the seeding CLI.

pub mod auth;
pub mod cli;
pub mod db;
pub mod room;
pub mod ws;

use crate::room::RoomManager;
use sqlx::SqlitePool;

#[derive(Debug, Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub secret: String,
    pub rooms: RoomManager,
}
