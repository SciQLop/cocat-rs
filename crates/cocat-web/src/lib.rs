//! The catalogue browser: a Leptos page that holds a live replica of a room.
//!
//! The modules declared first are plain logic, tested natively; the rest only exist in the
//! browser.

pub mod catalogues;
pub mod route;
pub mod session;
pub mod sync;

#[cfg(target_arch = "wasm32")]
pub mod app;
#[cfg(target_arch = "wasm32")]
mod connection;
#[cfg(target_arch = "wasm32")]
mod state;
#[cfg(target_arch = "wasm32")]
mod views;
