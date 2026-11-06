//! A client for cocat rooms.
//!
//! A [`Room`] owns one yrs document and its local cache, so it works offline: every local
//! transaction is written to the cache file at once, and [`Room::connect`] starts a background
//! task that exchanges the document with the server and reconnects on its own. Remote
//! transactions are reported as batches of [`Change`] values through [`Room::changes`].

pub mod changes;
mod login;
mod room;
mod sync;

pub use changes::Change;
pub use login::{LoginError, RoomInfo, login, rooms};
pub use room::{Room, RoomError, Status};
