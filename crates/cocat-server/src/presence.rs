//! Who is in a room: the Yjs awareness states its clients publish, kept by the room thread
//! so it can vouch for them.
//!
//! A client may only speak for one awareness id, the first it announces, and only if no
//! other account's client holds it. A page that reconnects before the server dropped its
//! old socket announces the same id from a new socket of the same account: the new socket
//! takes the id over. Its state's `user` field is always the account the
//! socket authenticated as, whatever the client wrote there, and the only other field kept
//! is `editing`: the server relays nothing a client could grow. A public catalogue relays
//! no presence at all.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use cocat_core::room_id::is_public_id;
use serde_json::{Map, Value, json};
use uuid::Uuid;
use yrs::ClientID;
use yrs::sync::Message;
use yrs::sync::awareness::{AwarenessUpdate, AwarenessUpdateEntry};
use yrs::sync::protocol::MSG_AWARENESS;
use yrs::updates::encoder::Encode;

use crate::room::ClientId;

/// Blue, orange, teal, purple, green, brown, pink and slate: each carries white text and
/// shows on both themes, and no two look alike. An account keeps its colour across sessions
/// and rooms.
pub use cocat_core::person::{COLORS, color};

/// An awareness frame carries one small state; a larger one is dropped before it is decoded.
const MAX_FRAME: usize = 4096;

/// An event uuid is 36 characters.
const MAX_EDITING: usize = 64;

/// One client's awareness: the id it speaks for and its last state, `None` once it said
/// it left.
struct State {
    id: ClientID,
    user: Uuid,
    entry: Option<AwarenessUpdateEntry>,
    clock: u32,
}

#[derive(Default)]
pub struct Presence {
    states: HashMap<ClientId, State>,
    /// A public catalogue is read by every account, so nobody's email is shown there.
    silent: bool,
}

impl Presence {
    pub fn for_room(room_id: &str) -> Self {
        Self {
            silent: is_public_id(room_id),
            ..Self::default()
        }
    }

    /// Takes a client's awareness update and answers the frame to relay to the others, if
    /// any part of it is the client's to make.
    pub fn update(
        &mut self,
        client: ClientId,
        user: Uuid,
        email: &str,
        update: AwarenessUpdate,
    ) -> Option<Bytes> {
        if self.silent {
            return None;
        }
        let id = self.owned_id(client, user, &update)?;
        let entry = update.clients.get(&id)?;
        let json = vouched(&entry.json, email)?;
        let entry = AwarenessUpdateEntry {
            clock: entry.clock,
            json: Arc::from(json),
        };
        let state = self.states.entry(client).or_insert(State {
            id,
            user,
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
    /// that update names when no other account's client holds it. A client of the same
    /// account that holds it loses it, so its departure does not remove the new state.
    fn owned_id(
        &mut self,
        client: ClientId,
        user: Uuid,
        update: &AwarenessUpdate,
    ) -> Option<ClientID> {
        if let Some(state) = self.states.get(&client) {
            return Some(state.id);
        }
        let mut ids = update.clients.keys();
        let id = *ids.next()?;
        if ids.next().is_some() {
            return None;
        }
        let holder = self.states.iter().find(|(_, state)| state.id == id);
        match holder.map(|(holder, state)| (*holder, state.user)) {
            None => Some(id),
            Some((holder, owner)) if owner == user => {
                self.states.remove(&holder);
                Some(id)
            }
            Some(_) => None,
        }
    }
}

/// Whether `frame` is an awareness frame too large to read.
pub fn oversized(frame: &[u8]) -> bool {
    frame.first() == Some(&MSG_AWARENESS) && frame.len() > MAX_FRAME
}

/// The state the server relays: the client's `editing` and the server's `user`. `null` (the
/// client leaving) passes as is, and anything but an object or `null` is refused.
fn vouched(json: &str, email: &str) -> Option<String> {
    match serde_json::from_str(json).ok()? {
        Value::Null => Some("null".to_owned()),
        Value::Object(state) => Some(
            json!({
                "editing": editing(&state),
                "user": { "email": email, "color": color(email) },
            })
            .to_string(),
        ),
        _ => None,
    }
}

/// The event the client says it edits; anything but a short string reads as none.
fn editing(state: &Map<String, Value>) -> Option<&str> {
    state
        .get("editing")?
        .as_str()
        .filter(|uuid| uuid.chars().count() <= MAX_EDITING)
}

fn frame(entries: impl IntoIterator<Item = (ClientID, AwarenessUpdateEntry)>) -> Bytes {
    let update = AwarenessUpdate {
        clients: entries.into_iter().collect(),
    };
    Bytes::from(Message::Awareness(update).encode_v1())
}

#[cfg(test)]
mod tests {
    use palette::color_difference::{EuclideanDistance, Wcag21RelativeContrast};
    use palette::{IntoColor, Oklab, Srgb};

    use super::{COLORS, color, oversized, vouched};

    /// The page's `--surface` in the dark theme.
    const DARK_SURFACE: &str = "#181e29";

    fn srgb(hex: &str) -> Srgb {
        hex.parse::<Srgb<u8>>().unwrap().into_format()
    }

    #[test]
    fn each_colour_carries_white_text_and_shows_on_both_themes() {
        let white = Srgb::new(1.0, 1.0, 1.0);
        for hex in COLORS {
            let colour = srgb(hex);
            assert!(
                colour.relative_contrast(white) >= 4.5,
                "{hex} under white text"
            );
            assert!(
                colour.relative_contrast(srgb(DARK_SURFACE)) >= 2.5,
                "{hex} on dark"
            );
        }
    }

    #[test]
    fn no_two_colours_look_alike() {
        let oklab = |hex: &str| -> Oklab { srgb(hex).into_linear().into_color() };
        for (index, a) in COLORS.iter().enumerate() {
            for b in &COLORS[index + 1..] {
                let distance = oklab(a).distance(oklab(b));
                assert!(distance >= 0.1, "{a} and {b} are {distance:.3} apart");
            }
        }
    }

    #[test]
    fn the_user_field_is_the_servers() {
        let state: serde_json::Value = serde_json::from_str(
            &vouched(r#"{"user":"me","editing":"e","extra":1}"#, "a@example.org").unwrap(),
        )
        .unwrap();
        assert_eq!(state["user"]["email"], "a@example.org");
        assert_eq!(state["editing"], "e");
        assert_eq!(state.as_object().unwrap().len(), 2);
        assert_eq!(vouched("null", "a@example.org").as_deref(), Some("null"));
        assert_eq!(vouched("[1]", "a@example.org"), None);
        assert_eq!(vouched("not json", "a@example.org"), None);
    }

    #[test]
    fn only_a_short_string_is_an_edited_event() {
        let editing = |json: &str| -> serde_json::Value {
            serde_json::from_str::<serde_json::Value>(&vouched(json, "a@example.org").unwrap())
                .unwrap()["editing"]
                .clone()
        };
        assert_eq!(
            editing(&format!(r#"{{"editing":"{}"}}"#, "e".repeat(64))),
            "e".repeat(64)
        );
        assert!(editing(&format!(r#"{{"editing":"{}"}}"#, "e".repeat(65))).is_null());
        assert!(editing(r#"{"editing":7}"#).is_null());
        assert!(editing("{}").is_null());
    }

    #[test]
    fn only_a_large_awareness_frame_is_oversized() {
        assert!(oversized(&[1; 4097]));
        assert!(!oversized(&[1; 4096]));
        assert!(!oversized(&[0; 4097]));
    }

    #[test]
    fn an_account_keeps_its_colour() {
        assert_eq!(color("a@example.org"), color("a@example.org"));
        assert!(COLORS.contains(&color("b@example.org")));
    }
}
