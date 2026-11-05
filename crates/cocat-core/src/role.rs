//! Roles granted on a room, in the same order of authority as the Python server.

use std::str::FromStr;

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    fn only_the_owner_and_the_editor_can_write() {
        assert!(Role::Owner.can_write());
        assert!(Role::Editor.can_write());
        assert!(!Role::Viewer.can_write());
    }

    #[test]
    fn an_unknown_role_name_is_rejected() {
        assert_eq!(
            "admin".parse::<Role>(),
            Err(UnknownRole("admin".to_owned()))
        );
    }
}
