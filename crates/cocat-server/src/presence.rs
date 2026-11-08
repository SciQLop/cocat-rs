//! Who is in a room: the Yjs awareness states its clients publish, kept by the room thread
//! so it can vouch for them.
//!
//! A client may only speak for one awareness id, the first it announces, and only if no
//! other client of the room holds it. Its state's `user` field is always the account the
//! socket authenticated as, whatever the client wrote there.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{Value, json};
use yrs::ClientID;
use yrs::sync::Message;
use yrs::sync::awareness::{AwarenessUpdate, AwarenessUpdateEntry};
use yrs::updates::encoder::Encode;

use crate::room::ClientId;

/// Readable on both themes; an account keeps its colour across sessions and rooms.
const COLORS: [&str; 8] = [
    "#2753b8", "#7c4dcc", "#c26a00", "#1f7a3d", "#b42318", "#0e7490", "#9d174d", "#4d5b16",
];

/// One client's awareness: the id it speaks for and its last state, `None` once it said
/// it left.
struct State {
    id: ClientID,
    entry: Option<AwarenessUpdateEntry>,
    clock: u32,
}

#[derive(Default)]
pub struct Presence {
    states: HashMap<ClientId, State>,
}

impl Presence {
    /// Takes a client's awareness update and answers the frame to relay to the others, if
    /// any part of it is the client's to make.
    pub fn update(
        &mut self,
        client: ClientId,
        email: &str,
        update: AwarenessUpdate,
    ) -> Option<Bytes> {
        let id = self.owned_id(client, &update)?;
        let entry = update.clients.get(&id)?;
        let json = with_user(&entry.json, email)?;
        let entry = AwarenessUpdateEntry {
            clock: entry.clock,
            json: Arc::from(json),
        };
        let state = self.states.entry(client).or_insert(State {
            id,
            entry: None,
            clock: 0,
        });
        state.clock = entry.clock;
        state.entry = (&*entry.json != "null").then(|| entry.clone());
        Some(frame([(id, entry)]))
    }

    /// Everyone's current state, for a client that just joined.
    pub fn snapshot(&self) -> Option<Bytes> {
        let entries: Vec<_> = self
            .states
            .values()
            .filter_map(|state| state.entry.clone().map(|entry| (state.id, entry)))
            .collect();
        (!entries.is_empty()).then(|| frame(entries))
    }

    /// Forgets the clients `present` no longer knows, and answers the update that removes
    /// them for the others: a `null` state one tick past their last clock.
    pub fn depart(&mut self, present: impl Fn(ClientId) -> bool) -> Option<Bytes> {
        let gone: Vec<ClientId> = self
            .states
            .keys()
            .copied()
            .filter(|client| !present(*client))
            .collect();
        let removals: Vec<_> = gone
            .into_iter()
            .filter_map(|client| self.states.remove(&client))
            .map(|state| {
                let entry = AwarenessUpdateEntry {
                    clock: state.clock.wrapping_add(1),
                    json: Arc::from("null"),
                };
                (state.id, entry)
            })
            .collect();
        (!removals.is_empty()).then(|| frame(removals))
    }

    /// The id `client` speaks for: the one it holds, or, on its first update, the single id
    /// that update names when no other client holds it.
    fn owned_id(&self, client: ClientId, update: &AwarenessUpdate) -> Option<ClientID> {
        if let Some(state) = self.states.get(&client) {
            return Some(state.id);
        }
        let mut ids = update.clients.keys();
        let id = *ids.next()?;
        let free = !self.states.values().any(|state| state.id == id);
        (ids.next().is_none() && free).then_some(id)
    }
}

/// The client's state with the server's `user`; `null` (the client leaving) passes as is,
/// and anything but an object or `null` is refused.
fn with_user(json: &str, email: &str) -> Option<String> {
    match serde_json::from_str(json).ok()? {
        Value::Null => Some("null".to_owned()),
        Value::Object(mut state) => {
            state.insert(
                "user".to_owned(),
                json!({ "email": email, "color": color(email) }),
            );
            Some(Value::Object(state).to_string())
        }
        _ => None,
    }
}

fn color(email: &str) -> &'static str {
    let hash = xxhash_rust::xxh3::xxh3_64(email.as_bytes());
    COLORS[(hash % COLORS.len() as u64) as usize]
}

fn frame(entries: impl IntoIterator<Item = (ClientID, AwarenessUpdateEntry)>) -> Bytes {
    let update = AwarenessUpdate {
        clients: entries.into_iter().collect(),
    };
    Bytes::from(Message::Awareness(update).encode_v1())
}

#[cfg(test)]
mod tests {
    use super::{COLORS, color, with_user};

    #[test]
    fn the_user_field_is_the_servers() {
        let state: serde_json::Value = serde_json::from_str(
            &with_user(r#"{"user":"me","editing":"e"}"#, "a@example.org").unwrap(),
        )
        .unwrap();
        assert_eq!(state["user"]["email"], "a@example.org");
        assert_eq!(state["editing"], "e");
        assert_eq!(with_user("null", "a@example.org").as_deref(), Some("null"));
        assert_eq!(with_user("[1]", "a@example.org"), None);
        assert_eq!(with_user("not json", "a@example.org"), None);
    }

    #[test]
    fn an_account_keeps_its_colour() {
        assert_eq!(color("a@example.org"), color("a@example.org"));
        assert!(COLORS.contains(&color("b@example.org")));
    }
}
