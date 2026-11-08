//! Roles granted on a room, in the same order of authority as the Python server.

use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Owner,
    Editor,
    Viewer,
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("unknown role: {0:?}")]
pub struct UnknownRole(pub String);

impl Role {
    pub fn can_write(&self) -> bool {
        matches!(self, Role::Owner | Role::Editor)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Editor => "editor",
            Role::Viewer => "viewer",
        }
    }

    /// Whether this member may change who has access: invite, revoke invitations, change
    /// roles, remove members and share with groups. The owner always may; an editor may while
    /// the owner lets editors share, as in Google Docs.
    pub fn may_share(&self, editors_can_share: bool) -> bool {
        match self {
            Role::Owner => true,
            Role::Editor => editors_can_share,
            Role::Viewer => false,
        }
    }

    /// Who may share a project, in words: "the owner", or "the owner and editors".
    pub fn sharers(editors_can_share: bool) -> &'static str {
        match editors_can_share {
            true => "the owner and editors",
            false => "the owner",
        }
    }

    /// Higher wins when one user is granted a role twice, as in the Python server:
    /// a member is never demoted by an add, only promoted.
    pub fn rank(&self) -> u8 {
        match self {
            Role::Owner => 3,
            Role::Editor => 2,
            Role::Viewer => 1,
        }
    }
}

impl FromStr for Role {
    type Err = UnknownRole;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "owner" => Ok(Role::Owner),
            "editor" => Ok(Role::Editor),
            "viewer" => Ok(Role::Viewer),
            other => Err(UnknownRole(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_round_trip_through_their_names() {
        for role in [Role::Owner, Role::Editor, Role::Viewer] {
            assert_eq!(role.as_str().parse(), Ok(role));
        }
    }

    #[test]
    fn json_names_are_the_role_names() {
        for role in [Role::Owner, Role::Editor, Role::Viewer] {
            let json = serde_json::to_string(&role).unwrap();
            assert_eq!(json, format!("\"{}\"", role.as_str()));
            assert_eq!(serde_json::from_str::<Role>(&json).unwrap(), role);
        }
    }

    #[test]
    fn the_owner_shares_and_editors_share_while_allowed() {
        assert!(Role::Owner.may_share(false));
        assert!(Role::Owner.may_share(true));
        assert!(Role::Editor.may_share(true));
        assert!(!Role::Editor.may_share(false));
        assert!(!Role::Viewer.may_share(true));
        assert!(!Role::Viewer.may_share(false));
    }

    #[test]
    fn only_the_owner_and_the_editor_can_write() {
        assert!(Role::Owner.can_write());
        assert!(Role::Editor.can_write());
        assert!(!Role::Viewer.can_write());
    }

    #[test]
    fn owners_rank_above_editors_above_viewers() {
        assert!(Role::Owner.rank() > Role::Editor.rank());
        assert!(Role::Editor.rank() > Role::Viewer.rank());
    }

    #[test]
    fn an_unknown_role_name_is_rejected() {
        assert_eq!(
            "admin".parse::<Role>(),
            Err(UnknownRole("admin".to_owned()))
        );
    }
}
