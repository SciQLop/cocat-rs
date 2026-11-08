//! The catalogue browser: a Leptos page that holds a live replica of a room.
//!
//! The modules declared first are plain logic, tested natively; the rest only exist in the
//! browser.

pub mod catalogues;
pub mod detail;
pub mod edit;
pub mod format;
pub mod help;
pub mod history;
pub mod layout;
pub mod period;
pub mod presence;
pub mod projects;
pub mod route;
pub mod rows;
pub mod rule_text;
pub mod session;
pub mod sharing;
pub mod stats;
pub mod sync;

#[cfg(test)]
mod fixture;

#[cfg(target_arch = "wasm32")]
mod api;
#[cfg(target_arch = "wasm32")]
pub mod app;
#[cfg(target_arch = "wasm32")]
mod connection;
#[cfg(target_arch = "wasm32")]
mod state;
#[cfg(target_arch = "wasm32")]
mod views;
