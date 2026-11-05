//! The cocat server: room websockets, authentication and the seeding CLI.

pub mod auth;
pub mod cli;
pub mod db;
pub mod room;

use axum::Router;
use axum::routing::post;
use sqlx::SqlitePool;

#[derive(Debug, Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub secret: String,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/auth/jwt/login", post(auth::login_handler))
        .with_state(state)
}
