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

/// What the page does once its socket closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterClose {
    /// Try again after the backoff.
    Reconnect,
    /// The account's role or membership changed: the server renders the page again with the
    /// new role, or says the project is gone.
    Reload,
}

pub fn after_close(code: u16) -> AfterClose {
    if code == cocat_core::ACCESS_CHANGED_CLOSE {
        AfterClose::Reload
    } else {
        AfterClose::Reconnect
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
/// How often a synced page asks the server for its state vector: the answer proves the
/// socket still carries frames. A half-open socket (the laptop slept and woke up on another
/// network) never closes on its own, and a browser cannot send a websocket ping.
pub const ASK_EVERY_MS: u32 = 20_000;
/// How long an ask may go unanswered before the socket counts as dead.
pub const ANSWER_WITHIN_MS: f64 = 30_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Beat {
    Wait,
    /// Send the state vector, and remember when.
    Ask,
    /// Give up on this socket and open another.
    Drop,
}

/// What the heartbeat does on a tick, from when it last asked and when a frame last came
/// (milliseconds). It times one ask, never the gap between ticks: a hidden tab's timers run
/// once a minute at most.
pub fn beat(synced: bool, asked: Option<f64>, heard: f64, now: f64) -> Beat {
    if !synced {
        return Beat::Wait;
    }
    match asked.filter(|asked| heard < *asked) {
        Some(asked) if now - asked > ANSWER_WITHIN_MS => Beat::Drop,
        Some(_) => Beat::Wait,
        None => Beat::Ask,
    }
}

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
            let _ = apply_state(doc, &update);
            Reception {
                replies: Vec::new(),
                synced: true,
            }
        }
        SyncMessage::Update(update) => {
            let _ = apply_state(doc, &update);
            Reception::default()
        }
    }
}

/// Applies an update from the server: a frame's, or the project's state downloaded before
/// the socket opens. What the codec or the document refuses (a proxy's error page) is
/// dropped: the next handshake repairs whatever it was missing.
pub fn apply_state(doc: &Doc, bytes: &[u8]) -> bool {
    Update::decode_v1(bytes)
        .is_ok_and(|update| doc.transact_mut_with(REMOTE).apply_update(update).is_ok())
}

/// How far the first load of a project got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loading {
    Downloading {
        received: u64,
        total: Option<u64>,
    },
    /// The download is in; the page reads it.
    Opening,
}

impl Loading {
    pub fn text(self) -> String {
        const MB: u64 = 1024 * 1024;
        match self {
            Loading::Downloading { received, total } if received >= MB => match total {
                Some(total) => format!("Loading… {} of {} MB", received / MB, total / MB),
                None => format!("Loading… {} MB", received / MB),
            },
            Loading::Downloading { .. } => "Loading…".to_owned(),
            Loading::Opening => "Opening…".to_owned(),
        }
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
    fn a_changed_access_reloads_the_page_and_anything_else_reconnects() {
        assert_eq!(
            after_close(cocat_core::ACCESS_CHANGED_CLOSE),
            AfterClose::Reload
        );
        assert_eq!(after_close(1008), AfterClose::Reconnect);
        assert_eq!(after_close(1006), AfterClose::Reconnect);
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

    #[test]
    fn a_downloaded_state_fills_the_replica_as_remote_and_the_hello_then_asks_for_nothing() {
        let server = server_with_a_catalogue();
        let state = server
            .transact()
            .encode_state_as_update_v1(&yrs::StateVector::default());
        let client = Doc::new();
        let origins = Rc::new(RefCell::new(Vec::new()));
        let seen = origins.clone();
        client
            .observe_update_v1("test", move |txn, _| seen.borrow_mut().push(is_remote(txn)))
            .unwrap();
        assert!(apply_state(&client, &state));
        assert_eq!(*origins.borrow(), [true]);
        let (step2, _) = deliver(&server, vec![hello(&client)]);
        let Ok(Message::Sync(SyncMessage::SyncStep2(diff))) = Message::decode_v1(&step2[0]) else {
            panic!("the server did not answer with its state");
        };
        assert_eq!(diff, [0, 0], "an empty update");
    }

    #[test]
    fn a_download_that_is_not_an_update_is_refused() {
        assert!(!apply_state(&Doc::new(), b"<html>proxy error</html>"));
    }

    #[test]
    fn the_page_says_how_far_the_download_got() {
        const MB: u64 = 1024 * 1024;
        let text = |received, total| Loading::Downloading { received, total }.text();
        assert_eq!(text(0, None), "Loading…");
        assert_eq!(text(3 * MB, None), "Loading… 3 MB");
        assert_eq!(text(12 * MB, Some(105 * MB)), "Loading… 12 of 105 MB");
        assert_eq!(text(MB / 2, Some(MB / 2)), "Loading…");
        assert_eq!(Loading::Opening.text(), "Opening…");
    }

    #[test]
    fn the_heartbeat_drops_a_socket_only_when_an_ask_goes_unanswered() {
        let answer = ANSWER_WITHIN_MS;
        assert_eq!(
            beat(false, None, 0.0, 1e9),
            Beat::Wait,
            "nothing to ask before the state is in"
        );
        assert_eq!(
            beat(true, None, 0.0, 1e9),
            Beat::Ask,
            "a hidden tab's late tick only asks"
        );
        assert_eq!(
            beat(true, Some(100.0), 150.0, 1e9),
            Beat::Ask,
            "the last ask was answered"
        );
        assert_eq!(
            beat(true, Some(100.0), 50.0, 100.0 + answer - 1.0),
            Beat::Wait
        );
        assert_eq!(
            beat(true, Some(100.0), 50.0, 100.0 + answer + 1.0),
            Beat::Drop
        );
    }
}
