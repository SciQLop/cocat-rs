//! The Yjs sync protocol as the room's server speaks it, without the socket: what to say
//! first, how to answer a frame, and how long to wait before reconnecting.

use std::time::Duration;

use yrs::sync::{Message, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, Origin, ReadTxn, Transact, TransactionMut, Update};

/// The origin of a transaction that applies what the server sent, so the observer that
/// sends local changes does not echo it back.
pub const REMOTE: &str = "remote";

const FIRST_DELAY: Duration = Duration::from_secs(1);
const LONGEST_DELAY: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Connecting,
    Live,
    Reconnecting,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Connecting => "Connecting…",
            Status::Live => "Live",
            Status::Reconnecting => "Reconnecting…",
        }
    }
}

/// The waits between reconnections: one second, doubling up to thirty.
#[derive(Debug)]
pub struct Backoff {
    next: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self { next: FIRST_DELAY }
    }
}

impl Backoff {
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = (delay * 2).min(LONGEST_DELAY);
        delay
    }

    pub fn reset(&mut self) {
        self.next = FIRST_DELAY;
    }
}

/// What one frame from the server asks of the page.
#[derive(Debug, Default)]
pub struct Reception {
    pub replies: Vec<Vec<u8>>,
    /// The server sent the state the page was missing: the replica is up to date.
    pub synced: bool,
}

/// The first frame of a connection: the replica's state vector, so the server answers
/// with what the page misses.
pub fn hello(doc: &Doc) -> Vec<u8> {
    Message::Sync(SyncMessage::SyncStep1(doc.transact().state_vector())).encode_v1()
}

pub fn update_frame(update: &[u8]) -> Vec<u8> {
    Message::Sync(SyncMessage::Update(update.to_vec())).encode_v1()
}

pub fn is_remote(txn: &TransactionMut<'_>) -> bool {
    txn.origin()
        .is_some_and(|origin| *origin == Origin::from(REMOTE))
}

/// Applies or answers one frame. Awareness frames are not handled here.
pub fn receive(doc: &Doc, bytes: &[u8]) -> Reception {
    let Ok(Message::Sync(message)) = Message::decode_v1(bytes) else {
        return Reception::default();
    };
    match message {
        SyncMessage::SyncStep1(wanted) => {
            // Edits made while the socket was down are in this diff.
            let diff = doc.transact().encode_state_as_update_v1(&wanted);
            Reception {
                replies: vec![Message::Sync(SyncMessage::SyncStep2(diff)).encode_v1()],
                synced: false,
            }
        }
        SyncMessage::SyncStep2(update) => {
            apply(doc, &update);
            Reception {
                replies: Vec::new(),
                synced: true,
            }
        }
        SyncMessage::Update(update) => {
            apply(doc, &update);
            Reception::default()
        }
    }
}

/// A frame the codec or the document refuses is dropped: the next handshake repairs
/// whatever it was missing.
fn apply(doc: &Doc, bytes: &[u8]) {
    if let Ok(update) = Update::decode_v1(bytes) {
        let _ = doc.transact_mut_with(REMOTE).apply_update(update);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    use cocat_core::model::{self, NewCatalogue};
    use yrs::{Doc, Transact};

    use super::*;

    fn seconds(backoff: &mut Backoff, count: usize) -> Vec<u64> {
        (0..count).map(|_| backoff.next_delay().as_secs()).collect()
    }

    #[test]
    fn the_backoff_doubles_from_one_second_to_thirty() {
        let mut backoff = Backoff::default();
        assert_eq!(seconds(&mut backoff, 7), [1, 2, 4, 8, 16, 30, 30]);
    }

    #[test]
    fn the_backoff_starts_over_after_a_reset() {
        let mut backoff = Backoff::default();
        seconds(&mut backoff, 4);
        backoff.reset();
        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
    }

    fn server_with_a_catalogue() -> Doc {
        let doc = Doc::new();
        let catalogue = NewCatalogue {
            uuid: "c1".into(),
            name: "Bow shock crossings".into(),
            ..NewCatalogue::default()
        };
        model::create_catalogue(&mut doc.transact_mut(), &catalogue).unwrap();
        doc
    }

    /// Delivers every frame to `to` and returns what `to` answered.
    fn deliver(to: &Doc, frames: Vec<Vec<u8>>) -> (Vec<Vec<u8>>, bool) {
        let mut replies = Vec::new();
        let mut synced = false;
        for frame in frames {
            let reception = receive(to, &frame);
            replies.extend(reception.replies);
            synced |= reception.synced;
        }
        (replies, synced)
    }

    #[test]
    fn the_handshake_brings_the_server_state_and_says_synced() {
        let server = server_with_a_catalogue();
        let client = Doc::new();
        let (step2, _) = deliver(&server, vec![hello(&client)]);
        let (replies, synced) = deliver(&client, step2);
        assert!(replies.is_empty());
        assert!(synced);
        let names: Vec<String> = model::catalogues(&client.transact())
            .into_iter()
            .map(|catalogue| catalogue.name)
            .collect();
        assert_eq!(names, ["Bow shock crossings"]);
    }

    #[test]
    fn the_client_answers_the_server_hello_with_its_offline_edits() {
        let server = Doc::new();
        let client = server_with_a_catalogue();
        let (step2, synced) = deliver(&client, vec![hello(&server)]);
        assert!(!synced);
        deliver(&server, step2);
        assert_eq!(model::catalogues(&server.transact()).len(), 1);
    }

    #[test]
    fn what_the_server_sends_is_applied_as_remote() {
        let client = Doc::new();
        let origins = Rc::new(RefCell::new(Vec::new()));
        let seen = origins.clone();
        client
            .observe_update_v1("test", move |txn, _| seen.borrow_mut().push(is_remote(txn)))
            .unwrap();
        let server = server_with_a_catalogue();
        let (step2, _) = deliver(&server, vec![hello(&client)]);
        deliver(&client, step2);
        assert_eq!(*origins.borrow(), [true]);
    }

    #[test]
    fn a_local_update_reaches_the_server() {
        let client = Doc::new();
        let frames = Rc::new(RefCell::new(Vec::new()));
        let sent = frames.clone();
        client
            .observe_update_v1("test", move |_, event| {
                sent.borrow_mut().push(update_frame(&event.update));
            })
            .unwrap();
        let catalogue = NewCatalogue {
            uuid: "c2".into(),
            name: "Training set".into(),
            ..NewCatalogue::default()
        };
        model::create_catalogue(&mut client.transact_mut(), &catalogue).unwrap();
        let server = Doc::new();
        deliver(&server, frames.take());
        assert_eq!(
            model::catalogues(&server.transact())[0].name,
            "Training set"
        );
    }

    #[test]
    fn a_frame_outside_the_sync_protocol_is_ignored() {
        let reception = receive(&Doc::new(), &[1, 0]);
        assert!(reception.replies.is_empty());
        assert!(!reception.synced);
    }
}
