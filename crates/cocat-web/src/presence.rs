//! Who else is in the room: the Yjs awareness states the server relays. The server writes
//! each state's `user` (email and colour) from the account behind the socket; a page only
//! says which event it is editing.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Value, json};
use yrs::Doc;
use yrs::sync::{Awareness, Message};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub email: String,
    pub initials: String,
    pub color: String,
    /// The uuid of the event the peer has open.
    pub editing: Option<String>,
}

pub struct Presence {
    awareness: Awareness,
}

impl Presence {
    pub fn new(doc: Doc) -> Self {
        Self {
            awareness: awareness(doc),
        }
    }

    /// Sets what this page is editing and answers the frame that tells the room.
    pub fn announce(&mut self, editing: Option<String>) -> Option<Vec<u8>> {
        self.awareness
            .set_local_state_raw(json!({ "editing": editing }).to_string());
        let own = self.awareness.client_id();
        let update = self.awareness.update_with_clients([own]).ok()?;
        Some(Message::Awareness(update).encode_v1())
    }

    /// Applies an awareness frame; answers whether anyone's state changed.
    pub fn receive(&mut self, bytes: &[u8]) -> bool {
        let Ok(Message::Awareness(update)) = Message::decode_v1(bytes) else {
            return false;
        };
        matches!(self.awareness.apply_update_summary(update), Ok(Some(_)))
    }

    /// The others in the room, one per account, by email.
    pub fn peers(&self) -> Vec<Peer> {
        let own = self.awareness.client_id();
        let mut peers: BTreeMap<String, Peer> = BTreeMap::new();
        let states = self.awareness.iter().filter(|(id, _)| *id != own);
        for peer in states.filter_map(|(_, state)| peer(state.data.as_deref()?)) {
            peers
                .entry(peer.email.clone())
                .and_modify(|kept| {
                    if kept.editing.is_none() {
                        kept.editing = peer.editing.clone();
                    }
                })
                .or_insert(peer);
        }
        peers.into_values().collect()
    }

    /// The socket closed: the others are unknown until the server's next snapshot. The
    /// server marks this page gone with its last clock plus one, so this page's next
    /// state must count past that to be taken.
    pub fn disconnected(&mut self) {
        let own = self.awareness.client_id();
        let carried = self.awareness.update_with_clients([own]).ok();
        let mut fresh = awareness(self.awareness.doc().clone());
        if let Some(mut update) = carried {
            for entry in update.clients.values_mut() {
                entry.clock += 1;
            }
            let _ = fresh.apply_update(update);
        }
        self.awareness = fresh;
    }
}

/// The rows others are editing, by event uuid.
pub fn editing(peers: &[Peer]) -> HashMap<String, Peer> {
    peers
        .iter()
        .filter_map(|peer| Some((peer.editing.clone()?, peer.clone())))
        .collect()
}

/// "carol.dupont@…" reads CD; a one-word name gives its first two letters.
pub fn initials(email: &str) -> String {
    let name = email.split_once('@').map_or(email, |(name, _)| name);
    let parts: Vec<&str> = name
        .split(['.', '_', '-', '+'])
        .filter(|part| !part.is_empty())
        .collect();
    let letters: String = match parts.as_slice() {
        [first, second, ..] => first
            .chars()
            .take(1)
            .chain(second.chars().take(1))
            .collect(),
        [only] => only.chars().take(2).collect(),
        [] => String::new(),
    };
    letters.to_uppercase()
}

// The states' update times are never read here, so a fixed clock does: it needs no
// browser API.
fn awareness(doc: Doc) -> Awareness {
    Awareness::with_clock(doc, || 0)
}

/// A state the server vouched for: it carries the account's `user`.
fn peer(json: &str) -> Option<Peer> {
    let state: Value = serde_json::from_str(json).ok()?;
    let user = state.get("user")?;
    let email = user.get("email")?.as_str()?.to_owned();
    Some(Peer {
        initials: initials(&email),
        color: user.get("color")?.as_str()?.to_owned(),
        editing: state
            .get("editing")
            .and_then(Value::as_str)
            .map(str::to_owned),
        email,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use yrs::sync::AwarenessUpdate;

    use super::*;

    /// What the server does to a frame before relaying it: its `user` replaces the client's.
    fn relayed(frame: &[u8], email: &str, color: &str) -> Vec<u8> {
        let Ok(Message::Awareness(mut update)) = Message::decode_v1(frame) else {
            panic!("not an awareness frame");
        };
        for entry in update.clients.values_mut() {
            let mut state: Value = serde_json::from_str(&entry.json).unwrap();
            if let Some(state) = state.as_object_mut() {
                state.insert("user".into(), json!({"email": email, "color": color}));
            }
            entry.json = Arc::from(state.to_string());
        }
        Message::Awareness(update).encode_v1()
    }

    /// The server's word that a client left: its state is `null`, one tick past its last.
    fn departure(frame: &[u8]) -> Vec<u8> {
        let Ok(Message::Awareness(mut update)) = Message::decode_v1(frame) else {
            panic!("not an awareness frame");
        };
        for entry in update.clients.values_mut() {
            entry.clock += 1;
            entry.json = Arc::from("null");
        }
        Message::Awareness(AwarenessUpdate {
            clients: update.clients,
        })
        .encode_v1()
    }

    fn carol(editing: Option<&str>) -> Peer {
        Peer {
            email: "carol.dupont@example.org".into(),
            initials: "CD".into(),
            color: "#7c4dcc".into(),
            editing: editing.map(str::to_owned),
        }
    }

    #[test]
    fn initials_come_from_the_name_before_the_at() {
        assert_eq!(initials("carol.dupont@example.org"), "CD");
        assert_eq!(initials("b_lavraud@example.org"), "BL");
        assert_eq!(initials("alice@example.org"), "AL");
        assert_eq!(initials("x@example.org"), "X");
    }

    #[test]
    fn a_peer_is_seen_with_the_server_identity_and_what_it_edits() {
        let (mut mine, mut theirs) = (Presence::new(Doc::new()), Presence::new(Doc::new()));
        let frame = theirs.announce(Some("e1".into())).unwrap();
        assert!(mine.receive(&relayed(&frame, "carol.dupont@example.org", "#7c4dcc")));
        assert_eq!(mine.peers(), [carol(Some("e1"))]);
        let frame = theirs.announce(None).unwrap();
        mine.receive(&relayed(&frame, "carol.dupont@example.org", "#7c4dcc"));
        assert_eq!(mine.peers(), [carol(None)]);
    }

    #[test]
    fn a_page_does_not_list_itself() {
        let mut mine = Presence::new(Doc::new());
        mine.announce(Some("e1".into())).unwrap();
        assert!(mine.peers().is_empty());
    }

    #[test]
    fn a_peer_the_server_says_left_disappears() {
        let (mut mine, mut theirs) = (Presence::new(Doc::new()), Presence::new(Doc::new()));
        let frame = theirs.announce(None).unwrap();
        mine.receive(&relayed(&frame, "carol.dupont@example.org", "#7c4dcc"));
        mine.receive(&departure(&frame));
        assert!(mine.peers().is_empty());
    }

    #[test]
    fn a_page_that_reconnects_is_seen_again_after_the_server_said_it_left() {
        let (mut mine, mut theirs) = (Presence::new(Doc::new()), Presence::new(Doc::new()));
        let frame = theirs.announce(Some("e1".into())).unwrap();
        mine.receive(&relayed(&frame, "carol.dupont@example.org", "#7c4dcc"));
        mine.receive(&departure(&frame));
        theirs.disconnected();
        let again = theirs.announce(Some("e1".into())).unwrap();
        mine.receive(&relayed(&again, "carol.dupont@example.org", "#7c4dcc"));
        assert_eq!(mine.peers(), [carol(Some("e1"))]);
    }

    #[test]
    fn a_disconnected_page_forgets_its_peers() {
        let (mut mine, mut theirs) = (Presence::new(Doc::new()), Presence::new(Doc::new()));
        mine.receive(&relayed(
            &theirs.announce(None).unwrap(),
            "carol.dupont@example.org",
            "#7c4dcc",
        ));
        mine.disconnected();
        assert!(mine.peers().is_empty());
    }

    #[test]
    fn two_pages_of_one_account_show_once() {
        let mut mine = Presence::new(Doc::new());
        for editing in [None, Some("e2".to_owned())] {
            let frame = Presence::new(Doc::new()).announce(editing).unwrap();
            mine.receive(&relayed(&frame, "carol.dupont@example.org", "#7c4dcc"));
        }
        assert_eq!(mine.peers(), [carol(Some("e2"))]);
    }

    #[test]
    fn rows_being_edited_take_their_editor() {
        let peers = [carol(Some("e1")), carol(None)];
        let rows = editing(&peers);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows["e1"].email, "carol.dupont@example.org");
    }

    #[test]
    fn a_frame_that_is_not_awareness_changes_nothing() {
        let mut mine = Presence::new(Doc::new());
        assert!(!mine.receive(&[0, 0, 1, 0]));
        assert!(!mine.receive(&[0xff]));
    }
}
