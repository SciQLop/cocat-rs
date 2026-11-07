//! Room storage shared by the cocat server and its tooling.

pub mod model;
pub mod role;
pub mod room_id;
pub mod store;
pub mod time;
pub mod votable;

/// The websocket subprotocol of the v2 room layout. A client that does not offer it would
/// write the first layout into a v2 room, so the server refuses it.
pub const SUBPROTOCOL: &str = "cocat.v2";
