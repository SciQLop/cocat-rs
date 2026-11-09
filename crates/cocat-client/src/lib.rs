//! A client for cocat rooms.
//!
//! A [`Room`] owns one yrs document and its local cache, so it works offline: every local
//! transaction is written to the cache file at once, and [`Room::connect`] starts a background
//! task that exchanges the document with the server and reconnects on its own. Remote
//! transactions are reported as batches of [`Change`] values through [`Room::changes`].

mod access;
pub mod changes;
mod history;
mod login;
mod projects;
mod room;
mod submissions;
mod sync;

pub use changes::Change;
pub use history::{commit, copy_version, history, name_version, restore, version_state};
pub use login::{Credentials, LoginError, RoomInfo, account_id, login, rooms};
pub use projects::{
    CreatedInvitation, INVITATION_DAYS, Invitation, Member, NewInvitation, NewPublicCatalogue,
    ProjectGroup, PublicCatalogue, PublicCatalogueChanges, create_project, create_public_catalogue,
    invitations, invite, members, project_groups, project_url, public_catalogues, remove_member,
    revoke_invitation, set_editors_can_share, set_role, update_public_catalogue,
};
pub use room::{Room, RoomError, Status};
pub use submissions::{
    Copied, NewSubmission, Submission, copy_public_catalogue, propose_change, review_submission,
    submissions, submit_catalogue, submit_update, withdraw_submission,
};
