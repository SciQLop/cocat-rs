//! The room the page holds: its replica, how far it has changed, and the connection.

use std::sync::Arc;

use leptos::prelude::*;
use yrs::{Doc, Transact, Transaction, TransactionMut};

use crate::edit::EditError;
use crate::session::RoomConfig;
use crate::sync::Status;

#[derive(Clone)]
pub struct Room {
    pub config: Arc<RoomConfig>,
    pub doc: Doc,
    pub status: RwSignal<Status>,
    revision: RwSignal<u64>,
}

impl Room {
    pub fn new(config: RoomConfig) -> Self {
        Self {
            config: Arc::new(config),
            doc: Doc::new(),
            status: RwSignal::new(Status::Connecting),
            revision: RwSignal::new(0),
        }
    }

    /// Reads the replica, and reads it again whenever it changes.
    pub fn read<R>(&self, read: impl FnOnce(&Transaction<'_>) -> R) -> R {
        self.revision.track();
        read(&self.doc.transact())
    }

    /// Reads the replica once, for an event handler or a form's first value.
    pub fn read_untracked<R>(&self, read: impl FnOnce(&Transaction<'_>) -> R) -> R {
        read(&self.doc.transact())
    }

    /// One edit, in one transaction of its own; the document's observer sends it.
    pub fn write<R>(
        &self,
        edit: impl FnOnce(&mut TransactionMut<'_>) -> Result<R, EditError>,
    ) -> Result<R, EditError> {
        edit(&mut self.doc.transact_mut())
    }

    /// Viewers get no edit controls at all.
    pub fn can_write(&self) -> bool {
        self.config.role.can_write()
    }

    pub fn changed(&self) {
        self.revision.update(|revision| *revision += 1);
    }
}
