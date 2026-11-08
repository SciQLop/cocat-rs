//! The role the server last gave this account in a room, kept in a file next to the cache so
//! that a viewer's room opened offline is read-only too.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cocat_core::role::Role;

use crate::room::lock;

pub(crate) struct Access {
    role: Mutex<Option<Role>>,
    /// `None` for a past version, which has no cache.
    file: Option<PathBuf>,
}

impl Access {
    /// The role remembered next to the cache file, if any connect ever learned one.
    pub(crate) fn remembered(cache_file: &Path) -> Access {
        let file = cache_file.with_extension("role");
        let role = fs::read_to_string(&file)
            .ok()
            .and_then(|text| text.trim().parse().ok());
        Access {
            role: Mutex::new(role),
            file: Some(file),
        }
    }

    pub(crate) fn none() -> Access {
        Access {
            role: Mutex::new(None),
            file: None,
        }
    }

    pub(crate) fn role(&self) -> Option<Role> {
        *lock(&self.role)
    }

    /// Records the role a handshake named, on disk too when it changed.
    pub(crate) fn learn(&self, role: Role) {
        let previous = lock(&self.role).replace(role);
        if previous == Some(role) {
            return;
        }
        let Some(file) = &self.file else {
            return;
        };
        if let Err(error) = fs::write(file, role.as_str()) {
            tracing::warn!(%error, path = %file.display(), "cannot remember the role");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Access;
    use cocat_core::role::Role;

    #[test]
    fn a_learned_role_is_remembered_by_the_next_open() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("Bepi MSA.y");
        assert_eq!(Access::remembered(&cache).role(), None);

        Access::remembered(&cache).learn(Role::Viewer);

        assert_eq!(Access::remembered(&cache).role(), Some(Role::Viewer));
        assert!(dir.path().join("Bepi MSA.role").exists());
    }
}
