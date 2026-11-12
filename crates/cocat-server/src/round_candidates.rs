//! A round's candidates: a frozen copy of a catalogue its creator can read, written once
//! into a proposal document of the round's project.

use cocat_core::model;
use uuid::Uuid;
use yrs::{Doc, Transact};

use crate::api::{ApiError, membership};
use crate::history_api::encoded;
use crate::room::{Author, RoomError};
use crate::rounds::{CandidateSource, Candidates};
use crate::submissions_api::{extract_alone, recorded_copy};
use crate::{AppState, db, documents};

/// The candidates ready to be written into their document.
pub(crate) struct Copied {
    pub candidates: Candidates,
    update: Vec<u8>,
    allowance: u64,
}

/// Copies the source catalogue's events outside the trash, their uuids kept, as `user`
/// copying them out of the source project, and checks the copy fits `room_id`'s storage.
/// 404 when `user` cannot read the source project or it holds no such catalogue.
pub(crate) async fn copy(
    state: &AppState,
    room_id: &str,
    source: &CandidateSource,
    user: &db::User,
) -> Result<Copied, ApiError> {
    membership(state, &source.project, user.id).await?;
    let catalogue = source.catalogue.clone();
    let (snapshot, count) = state
        .rooms
        .read(&source.project, move |doc| live_copy(doc, &catalogue))
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Catalogue {} not found", source.catalogue)))?;
    let update = recorded_copy(state, &source.project, &user.email, snapshot).await?;
    let doc = Uuid::new_v4();
    let allowance = state
        .rooms
        .allowance(&state.pool, &documents::key(room_id, doc))
        .await?;
    if u64::try_from(update.len()).unwrap_or(u64::MAX) > allowance {
        return Err(RoomError::Full.into());
    }
    Ok(Copied {
        candidates: Candidates {
            doc,
            source: source.clone(),
            count,
        },
        update,
        allowance,
    })
}

/// Writes the candidates into their document, as `by`'s write.
pub(crate) async fn write(
    state: &AppState,
    room_id: &str,
    by: Uuid,
    copied: Copied,
) -> Result<(), ApiError> {
    let key = documents::key(room_id, copied.candidates.doc);
    state
        .rooms
        .write_as(&key, copied.allowance, Author::Client(by), copied.update)
        .await?;
    Ok(())
}

/// The catalogue alone without its events in the trash, and how many events it keeps.
fn live_copy(doc: &Doc, catalogue: &str) -> Option<(Vec<u8>, i64)> {
    let (_, alone) = extract_alone(doc, catalogue)?;
    let count = model::live_event_count(&alone.transact());
    Some((encoded(&alone), i64::try_from(count).unwrap_or(i64::MAX)))
}
