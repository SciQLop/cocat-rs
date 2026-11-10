//! Presence: the room relays Yjs awareness between its clients, under the identity the server
//! knows rather than the one a client claims.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cocat_core::role::Role;
use cocat_server::room::{Inbox, RoomHandle, RoomManager, outbox};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::timeout;
use uuid::Uuid;
use yrs::ClientID;
use yrs::sync::awareness::{AwarenessUpdate, AwarenessUpdateEntry};
use yrs::sync::{Message, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;

const WAIT: Duration = Duration::from_secs(5);

struct Room {
    handle: RoomHandle,
    _dir: TempDir,
}

fn open() -> Room {
    open_named("lab")
}

fn open_named(room_id: &str) -> Room {
    let dir = TempDir::new().unwrap();
    let manager = RoomManager::new(dir.path().to_owned(), None);
    Room {
        handle: manager.get_or_open(room_id).unwrap(),
        _dir: dir,
    }
}

fn join(room: &Room, client: u64, email: &str, role: Role) -> Inbox {
    join_as(room, client, Uuid::new_v4(), email, role)
}

fn join_as(room: &Room, client: u64, user: Uuid, email: &str, role: Role) -> Inbox {
    let (out, inbox) = outbox();
    room.handle
        .attach(client, user, email.to_owned(), role, out)
        .unwrap();
    inbox
}

fn awareness(id: u64, clock: u32, state: Value) -> Bytes {
    let update = AwarenessUpdate {
        clients: HashMap::from([(
            ClientID::new(id),
            AwarenessUpdateEntry {
                clock,
                json: Arc::from(state.to_string()),
            },
        )]),
    };
    Bytes::from(Message::Awareness(update).encode_v1())
}

/// The next awareness update the client receives, skipping the sync handshake.
async fn next_awareness(inbox: &mut Inbox) -> HashMap<u64, (u32, Value)> {
    loop {
        let frame = timeout(WAIT, inbox.frames.recv())
            .await
            .expect("no awareness frame arrived")
            .expect("the room closed the client");
        if let Message::Awareness(update) = Message::decode_v1(&frame).unwrap() {
            return update
                .clients
                .into_iter()
                .map(|(id, entry)| {
                    (
                        id.get(),
                        (entry.clock, serde_json::from_str(&entry.json).unwrap()),
                    )
                })
                .collect();
        }
    }
}

/// Proves nothing else is queued: a sync request is answered, and that answer comes first.
async fn nothing_but_the_answer(room: &Room, client: u64, inbox: &mut Inbox) {
    let step1 = Message::Sync(SyncMessage::SyncStep1(Default::default())).encode_v1();
    room.handle.frame(client, Bytes::from(step1));
    loop {
        let frame = timeout(WAIT, inbox.frames.recv()).await.unwrap().unwrap();
        match Message::decode_v1(&frame).unwrap() {
            Message::Sync(SyncMessage::SyncStep2(_)) => return,
            Message::Sync(SyncMessage::SyncStep1(_)) => continue,
            other => panic!("expected no other frame, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn awareness_is_relayed_with_the_server_identity() {
    let room = open();
    let mut alice = join(&room, 1, "alice@example.org", Role::Editor);
    let mut bob = join(&room, 2, "bob@example.org", Role::Viewer);

    room.handle.frame(
        2,
        awareness(
            700,
            1,
            json!({"user": {"email": "alice@example.org", "color": "#000000"}, "editing": "e1"}),
        ),
    );

    let seen = next_awareness(&mut alice).await;
    let (clock, state) = &seen[&700];
    assert_eq!(*clock, 1);
    assert_eq!(state["editing"], "e1");
    assert_eq!(state["user"]["email"], "bob@example.org");
    let color = state["user"]["color"].as_str().unwrap();
    assert!(
        color.len() == 7 && color.starts_with('#') && color != "#000000",
        "{color}"
    );
    nothing_but_the_answer(&room, 2, &mut bob).await;
}

#[tokio::test]
async fn a_client_only_speaks_for_its_own_awareness_id() {
    let room = open();
    let mut alice = join(&room, 1, "alice@example.org", Role::Editor);
    let mut bob = join(&room, 2, "bob@example.org", Role::Editor);
    room.handle
        .frame(1, awareness(100, 1, json!({"editing": "a"})));
    assert_eq!(next_awareness(&mut bob).await[&100].1["editing"], "a");

    room.handle
        .frame(2, awareness(100, 2, json!({"editing": "spoof"})));
    room.handle
        .frame(2, awareness(200, 1, json!({"editing": "b"})));
    room.handle
        .frame(2, awareness(201, 1, json!({"editing": "second id"})));

    let seen = next_awareness(&mut alice).await;
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[&200].1["editing"], "b");
    assert_eq!(seen[&200].1["user"]["email"], "bob@example.org");
    nothing_but_the_answer(&room, 1, &mut alice).await;
}

#[tokio::test]
async fn a_joining_client_sees_who_is_already_there() {
    let room = open();
    let mut alice = join(&room, 1, "alice@example.org", Role::Editor);
    room.handle
        .frame(1, awareness(100, 3, json!({"editing": null})));
    nothing_but_the_answer(&room, 1, &mut alice).await;

    let mut carol = join(&room, 3, "carol@example.org", Role::Viewer);

    let seen = next_awareness(&mut carol).await;
    assert_eq!(seen[&100].0, 3);
    assert_eq!(seen[&100].1["user"]["email"], "alice@example.org");
}

#[tokio::test]
async fn a_closed_client_disappears_from_awareness() {
    let room = open();
    let mut alice = join(&room, 1, "alice@example.org", Role::Editor);
    let _bob = join(&room, 2, "bob@example.org", Role::Editor);
    room.handle
        .frame(2, awareness(200, 4, json!({"editing": "b"})));
    assert!(next_awareness(&mut alice).await.contains_key(&200));

    room.handle.detach(2).await;

    let seen = next_awareness(&mut alice).await;
    let (clock, state) = &seen[&200];
    assert!(*clock > 4, "the removal must win over the last state");
    assert_eq!(*state, Value::Null);
}

#[tokio::test]
async fn an_oversized_awareness_frame_is_dropped() {
    let room = open();
    let mut alice = join(&room, 1, "alice@example.org", Role::Editor);
    let _bob = join(&room, 2, "bob@example.org", Role::Viewer);

    let padding = "x".repeat(5000);
    room.handle.frame(
        2,
        awareness(200, 1, json!({"editing": "b", "padding": padding})),
    );

    nothing_but_the_answer(&room, 1, &mut alice).await;
}

#[tokio::test]
async fn only_the_editing_field_and_the_servers_user_are_relayed() {
    let room = open();
    let mut alice = join(&room, 1, "alice@example.org", Role::Editor);
    let _bob = join(&room, 2, "bob@example.org", Role::Viewer);

    room.handle.frame(
        2,
        awareness(
            200,
            1,
            json!({"editing": "b", "cursor": [1, 2], "name": "Mallory"}),
        ),
    );
    let seen = next_awareness(&mut alice).await;
    let state = seen[&200].1.as_object().unwrap();
    let mut keys: Vec<&str> = state.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["editing", "user"]);
    assert_eq!(state["editing"], "b");

    room.handle
        .frame(2, awareness(200, 2, json!({"editing": "e".repeat(65)})));
    assert_eq!(
        next_awareness(&mut alice).await[&200].1["editing"],
        Value::Null
    );
}

#[tokio::test]
async fn a_page_that_reconnects_takes_its_awareness_id_over() {
    let room = open();
    let alice = Uuid::new_v4();
    let _old = join_as(&room, 1, alice, "alice@example.org", Role::Editor);
    let mut bob = join(&room, 2, "bob@example.org", Role::Editor);
    room.handle
        .frame(1, awareness(100, 1, json!({"editing": "before"})));
    assert_eq!(next_awareness(&mut bob).await[&100].1["editing"], "before");

    let _new = join_as(&room, 3, alice, "alice@example.org", Role::Editor);
    room.handle
        .frame(3, awareness(100, 3, json!({"editing": "after"})));
    assert_eq!(next_awareness(&mut bob).await[&100].1["editing"], "after");

    room.handle.detach(1).await;
    nothing_but_the_answer(&room, 2, &mut bob).await;
}

#[tokio::test]
async fn a_public_catalogue_relays_no_presence_either_way() {
    let room = open_named("public.shocks");
    let mut curator = join(&room, 1, "curator@example.org", Role::Editor);
    room.handle
        .frame(1, awareness(100, 1, json!({"editing": "e1"})));
    let mut reader = join(&room, 2, "reader@example.org", Role::Viewer);
    room.handle
        .frame(2, awareness(200, 1, json!({"editing": "e2"})));

    nothing_but_the_answer(&room, 2, &mut reader).await;
    nothing_but_the_answer(&room, 1, &mut curator).await;
}

fn open_document() -> Room {
    open_named(&format!("lab/{}", Uuid::new_v4()))
}

#[tokio::test]
async fn a_notebook_keeps_cursors_and_selections() {
    let room = open_document();
    let mut alice = join(&room, 1, "alice@example.org", Role::Editor);
    let _bob = join(&room, 2, "bob@example.org", Role::Editor);

    let state = json!({
        "user": {"name": "Mallory", "initials": "M"},
        "cursors": [{"anchor": 1, "head": 4}],
        "selection": {"start": 2, "end": 9},
        "current": "cell-3",
    });
    room.handle.frame(2, awareness(300, 1, state.clone()));

    let seen = next_awareness(&mut alice).await;
    let relayed = seen[&300].1.as_object().unwrap();
    for field in ["cursors", "selection", "current"] {
        assert_eq!(relayed[field], state[field], "{field}");
    }
    assert_eq!(relayed["user"]["email"], "bob@example.org");
    assert_eq!(relayed["user"]["name"], "bob");
    assert_eq!(relayed["user"]["display_name"], "bob");
    assert!(relayed["user"]["color"].is_string());
    assert!(relayed["user"].get("initials").is_none());
}

#[tokio::test]
async fn a_notebook_state_cannot_claim_another_account() {
    let room = open_document();
    let mut alice = join(&room, 1, "alice@example.org", Role::Editor);
    let _bob = join(&room, 2, "bob@example.org", Role::Editor);

    let claim = json!({"email": "alice@example.org", "name": "alice", "color": "#000000"});
    room.handle
        .frame(2, awareness(301, 1, json!({"user": claim, "current": "c"})));

    let seen = next_awareness(&mut alice).await;
    let user = &seen[&301].1["user"];
    assert_eq!(user["email"], "bob@example.org");
    assert_eq!(user["name"], "bob");
    assert_ne!(user["color"], "#000000");
}

#[tokio::test]
async fn a_notebook_frame_over_16_kb_is_dropped() {
    let room = open_document();
    let mut alice = join(&room, 1, "alice@example.org", Role::Editor);
    let _bob = join(&room, 2, "bob@example.org", Role::Editor);

    let big = "x".repeat(17 * 1024);
    room.handle
        .frame(2, awareness(302, 1, json!({"cursors": big})));
    nothing_but_the_answer(&room, 1, &mut alice).await;

    let fits = "x".repeat(8 * 1024);
    room.handle
        .frame(2, awareness(302, 2, json!({"cursors": fits})));
    assert_eq!(
        next_awareness(&mut alice).await[&302].1["cursors"],
        json!(fits)
    );
}

#[tokio::test]
async fn a_catalogue_still_keeps_only_user_and_editing() {
    let room = open();
    let mut alice = join(&room, 1, "alice@example.org", Role::Editor);
    let _bob = join(&room, 2, "bob@example.org", Role::Editor);

    room.handle.frame(
        2,
        awareness(303, 1, json!({"editing": "b", "cursors": [1]})),
    );
    let seen = next_awareness(&mut alice).await;
    let mut keys: Vec<&str> = seen[&303]
        .1
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["editing", "user"]);
    assert!(seen[&303].1["user"].get("name").is_none());
}
