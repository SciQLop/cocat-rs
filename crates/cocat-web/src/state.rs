//! The room the page holds: its replica, how far it has changed, and the connection.

use std::sync::Arc;

use leptos::prelude::*;
use yrs::{Doc, Transact, Transaction, TransactionMut};

use crate::edit::EditError;
use crate::presence::Peer;
use crate::session::RoomConfig;
use crate::sync::{Loading, Status};

#[derive(Clone)]
pub struct Room {
    pub config: Arc<RoomConfig>,
    pub doc: Doc,
    pub status: RwSignal<Status>,
    /// Set while the page first loads the project, until it has the server's state.
    pub loading: RwSignal<Option<Loading>>,
    /// The others in the room now.
    pub peers: RwSignal<Vec<Peer>>,
    /// The event this page has open for editing, which the others see.
    pub editing: RwSignal<Option<String>>,
    revision: RwSignal<u64>,
}

impl Room {
    pub fn new(config: RoomConfig) -> Self {
        Self {
            config: Arc::new(config),
            doc: Doc::new(),
            status: RwSignal::new(Status::Connecting),
            loading: RwSignal::new(None),
            peers: RwSignal::new(Vec::new()),
            editing: RwSignal::new(None),
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

    /// Runs the caller again whenever the replica changes.
    pub fn track(&self) {
        self.revision.track();
    }

    pub fn changed(&self) {
        self.revision.update(|revision| *revision += 1);
    }
}
