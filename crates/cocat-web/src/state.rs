//! The room the page holds: its replica, how far it has changed, and the connection.

use std::sync::Arc;

use leptos::prelude::*;
use yrs::{Doc, Transact, Transaction};

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

    pub fn changed(&self) {
        self.revision.update(|revision| *revision += 1);
    }
}
