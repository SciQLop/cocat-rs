//! The catalogue browser: a Leptos page that holds a live replica of a room.
//!
//! The modules declared first are plain logic, tested natively; the rest only exist in the
//! browser.

pub mod agent_tools;
pub mod catalogues;
pub mod detail;
pub mod edit;
pub mod format;
pub mod help;
pub mod highlight;
pub mod history;
pub mod home;
pub mod layout;
pub mod links;
pub mod markdown;
pub mod notebook;
pub mod period;
pub mod presence;
pub mod preview;
pub mod projects;
pub mod prose;
pub mod public;
pub mod review;
pub mod rounds;
pub mod route;
pub mod rows;
pub mod rule_text;
pub mod search;
pub mod session;
pub mod sharing;
pub mod stats;
pub mod submissions;
pub mod sync;
pub mod transfer;
pub mod upload;
pub mod workspace;

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
