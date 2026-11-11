//! Room storage shared by the cocat server and its tooling.

pub mod csv_table;
pub mod files;
pub mod model;
pub mod person;
pub mod query;
pub mod restore;
pub mod role;
pub mod room_id;
pub mod round;
pub mod rules;
pub mod shape_json;
pub mod store;
pub mod tfcat;
pub mod time;
pub mod tscat_json;
pub mod votable;

/// The websocket subprotocol of the v2 room layout. A client that does not offer it would
/// write the first layout into a v2 room, so the server refuses it.
pub const SUBPROTOCOL: &str = "cocat.v2";

/// The header of the websocket upgrade answer naming the account's role in the room, so a
/// client knows before it writes whether the server would drop its changes. Older clients
/// ignore it.
pub const ROLE_HEADER: &str = "cocat-role";

/// The header of a project's state answer giving its size before compression, so a page can
/// show how far the download got.
pub const STATE_BYTES_HEADER: &str = "cocat-state-bytes";

/// The websocket close code for "this account's access to the project changed": its role
/// changed, it left the project, or the project went away. The session is still good, so a
/// client reconnects at once and the handshake tells it what it may do now. `1008` stays the
/// code for a session that ended.
pub const ACCESS_CHANGED_CLOSE: u16 = 4403;
