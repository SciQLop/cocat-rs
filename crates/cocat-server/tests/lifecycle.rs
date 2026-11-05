use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use cocat_core::role::Role;
use cocat_server::lifecycle::{self, LifecycleError};
use cocat_server::room::RoomManager;
use cocat_server::{AppState, auth, db, ws};
use futures_util::{SinkExt, StreamExt};
use tempfile::TempDir;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use uuid::Uuid;
use yrs::sync::{Message, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, Map, ReadTxn, StateVector, Transact};

const SECRET: &str = "test-secret";
const WAIT: Duration = Duration::from_secs(10);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Fixture {
    addr: SocketAddr,
    state: AppState,
    rooms_dir: PathBuf,
    _dir: TempDir,
    _server: JoinHandle<()>,
}

impl Fixture {
    async fn start() -> Self {
        let dir = TempDir::new().unwrap();
        let rooms_dir = dir.path().join("rooms");
        fs::create_dir_all(&rooms_dir).unwrap();
        let pool = db::open(&dir.path().join("test.db")).await.unwrap();
        let state = AppState {
            pool,
            secret: SECRET.to_owned(),
            rooms: RoomManager::new(rooms_dir.clone(), None),
            before_attach: None,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let served = state.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, ws::router(served)).await.unwrap();
        });
        Self {
            addr,
            state,
            rooms_dir,
            _dir: dir,
            _server: server,
        }
    }

    async fn user(&self, email: &str) -> Uuid {
        db::create_user(&self.state.pool, email, "hunter2", false)
            .await
            .unwrap()
    }

    async fn room(&self, room_id: &str, owner: Uuid) {
        db::create_room(&self.state.pool, owner, room_id, room_id)
            .await
            .unwrap();
    }

    async fn member(&self, room_id: &str, email: &str, role: Role) -> Uuid {
        let user = self.user(email).await;
        db::add_member(&self.state.pool, room_id, user, role)
            .await
            .unwrap();
        user
    }

    fn token(&self, user: Uuid) -> String {
        auth::mint_token(user, SECRET, SystemTime::now()).unwrap()
    }

    async fn handshake(&self, room_id: &str, token: Option<&str>) -> Result<Socket, u16> {
        let mut request = format!("ws://{}/room/{room_id}", self.addr)
            .into_client_request()
            .unwrap();
        if let Some(token) = token {
            request.headers_mut().insert(
                "cookie",
                format!("fastapiusersauth={token}").parse().unwrap(),
            );
        }
        match connect_async(request).await {
            Ok((socket, response)) => {
                assert_eq!(response.status(), 101);
                Ok(socket)
            }
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                Err(response.status().as_u16())
            }
            Err(error) => panic!("the handshake failed: {error}"),
        }
    }

    async fn connect(&self, room_id: &str, user: Uuid) -> Socket {
        self.handshake(room_id, Some(&self.token(user)))
            .await
            .unwrap_or_else(|status| panic!("the handshake was refused with {status}"))
    }
}

fn update_with(key: &str, value: &str) -> Vec<u8> {
    let doc = Doc::new();
    doc.get_or_insert_map("content")
        .insert(&mut doc.transact_mut(), key, value);
    doc.transact()
        .encode_state_as_update_v1(&StateVector::default())
}

fn sync_frame(message: Message) -> WsMessage {
    WsMessage::Binary(Bytes::from(message.encode_v1()))
}

fn update_frame(payload: &[u8]) -> WsMessage {
    sync_frame(Message::Sync(SyncMessage::Update(payload.to_vec())))
}

fn sync_step1() -> Message {
    Message::Sync(SyncMessage::SyncStep1(StateVector::default()))
}

async fn next_ws(socket: &mut Socket) -> WsMessage {
    timeout(WAIT, socket.next())
        .await
        .expect("the socket went quiet")
        .expect("the socket failed")
        .expect("the socket closed")
}

async fn next_message(socket: &mut Socket) -> Message {
    match next_ws(socket).await {
        WsMessage::Binary(bytes) => {
            Message::decode_v1(&bytes).expect("the server sent a bad frame")
        }
        other => panic!("expected a binary frame, got {other:?}"),
    }
}

/// Reads past the frames a join produces until the server closes the socket.
async fn expect_close(socket: &mut Socket, code: u16) {
    loop {
        match next_ws(socket).await {
            WsMessage::Close(frame) => {
                assert_eq!(
                    frame.map(|frame| u16::from(frame.code)),
                    Some(code),
                    "the socket closed with the wrong code"
                );
                return;
            }
            WsMessage::Binary(_) => {}
            other => panic!("expected a close, got {other:?}"),
        }
    }
}

async fn join(socket: &mut Socket) {
    assert!(matches!(
        next_message(socket).await,
        Message::Sync(SyncMessage::SyncStep1(_))
    ));
}

fn content(payload: &[u8], key: &str) -> Option<String> {
    let doc = Doc::new();
    doc.transact_mut()
        .apply_update(yrs::Update::decode_v1(payload).unwrap())
        .unwrap();
    let map = doc.get_or_insert_map("content");
    map.get(&doc.transact(), key)
        .map(|value| value.to_string(&doc.transact()))
}

#[tokio::test]
async fn removed_member_is_closed_1008_and_refused_403_on_reconnect() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room("room-kick", owner).await;
    let editor = fixture
        .member("room-kick", "editor@example.org", Role::Editor)
        .await;
    let mut socket = fixture.connect("room-kick", editor).await;
    join(&mut socket).await;

    lifecycle::remove_member(&fixture.state, "room-kick", owner, editor)
        .await
        .unwrap();

    expect_close(&mut socket, 1008).await;
    let refused = fixture
        .handshake("room-kick", Some(&fixture.token(editor)))
        .await
        .err();
    assert_eq!(refused, Some(403));
}

#[tokio::test]
async fn delete_room_closes_every_member_and_removes_the_file() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room("room-gone", owner).await;
    let editor = fixture
        .member("room-gone", "editor@example.org", Role::Editor)
        .await;
    let mut owner_socket = fixture.connect("room-gone", owner).await;
    let mut editor_socket = fixture.connect("room-gone", editor).await;
    join(&mut owner_socket).await;
    join(&mut editor_socket).await;
    editor_socket
        .send(update_frame(&update_with("key", "value")))
        .await
        .unwrap();
    next_message(&mut owner_socket).await;
    let path = fixture.rooms_dir.join("room-gone.y");
    assert!(path.exists());

    lifecycle::delete_room(&fixture.state, "room-gone", owner)
        .await
        .unwrap();

    expect_close(&mut owner_socket, 1008).await;
    expect_close(&mut editor_socket, 1008).await;
    assert!(!path.exists(), "the deleted room left its file behind");
    assert_eq!(fixture.state.rooms.open_rooms(), 0);
    let remaining = db::role_in(&fixture.state.pool, "room-gone", editor)
        .await
        .unwrap();
    assert_eq!(remaining, None);
}

#[tokio::test]
async fn downgrading_a_member_closes_their_socket_and_the_next_one_is_read_only() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room("room-down", owner).await;
    let editor = fixture
        .member("room-down", "editor@example.org", Role::Editor)
        .await;
    let mut socket = fixture.connect("room-down", editor).await;
    join(&mut socket).await;

    lifecycle::set_member_role(&fixture.state, "room-down", owner, editor, Role::Viewer)
        .await
        .unwrap();

    expect_close(&mut socket, 1008).await;

    let mut viewer = fixture.connect("room-down", editor).await;
    join(&mut viewer).await;
    viewer
        .send(update_frame(&update_with("key", "value")))
        .await
        .unwrap();
    viewer.send(sync_frame(sync_step1())).await.unwrap();
    let reply = match next_message(&mut viewer).await {
        Message::Sync(SyncMessage::SyncStep2(update)) => update,
        other => panic!("expected a step2, got {other:?}"),
    };
    assert_eq!(content(&reply, "key"), None);
}

#[tokio::test]
async fn changing_a_role_does_not_close_other_members() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room("room-role", owner).await;
    let first = fixture
        .member("room-role", "first@example.org", Role::Editor)
        .await;
    let second = fixture
        .member("room-role", "second@example.org", Role::Editor)
        .await;
    let mut owner_socket = fixture.connect("room-role", owner).await;
    let mut first_socket = fixture.connect("room-role", first).await;
    let mut second_socket = fixture.connect("room-role", second).await;
    join(&mut owner_socket).await;
    join(&mut first_socket).await;
    join(&mut second_socket).await;

    lifecycle::set_member_role(&fixture.state, "room-role", owner, first, Role::Viewer)
        .await
        .unwrap();

    expect_close(&mut first_socket, 1008).await;
    second_socket
        .send(update_frame(&update_with("key", "value")))
        .await
        .unwrap();
    assert!(matches!(
        next_message(&mut owner_socket).await,
        Message::Sync(SyncMessage::Update(_))
    ));
    assert!(matches!(
        next_message(&mut second_socket).await,
        Message::Sync(SyncMessage::Update(_))
    ));
}

#[tokio::test]
async fn an_editor_cannot_change_a_role() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room("room-forbidden", owner).await;
    let editor = fixture
        .member("room-forbidden", "editor@example.org", Role::Editor)
        .await;
    let viewer = fixture
        .member("room-forbidden", "viewer@example.org", Role::Viewer)
        .await;

    let error = lifecycle::set_member_role(
        &fixture.state,
        "room-forbidden",
        editor,
        viewer,
        Role::Editor,
    )
    .await
    .unwrap_err();

    assert!(matches!(error, LifecycleError::Forbidden), "{error:?}");
}

#[tokio::test]
async fn a_non_member_actor_gets_not_found() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room("room-outsider", owner).await;
    let editor = fixture
        .member("room-outsider", "editor@example.org", Role::Editor)
        .await;
    let stranger = fixture.user("stranger@example.org").await;

    let error = lifecycle::remove_member(&fixture.state, "room-outsider", stranger, editor)
        .await
        .unwrap_err();

    assert!(matches!(error, LifecycleError::NotFound), "{error:?}");
}

#[tokio::test]
async fn a_kick_does_not_reach_the_same_user_in_another_room() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room("room-one", owner).await;
    fixture.room("room-two", owner).await;
    let member = fixture
        .member("room-one", "member@example.org", Role::Editor)
        .await;
    db::add_member(&fixture.state.pool, "room-two", member, Role::Editor)
        .await
        .unwrap();
    let mut first = fixture.connect("room-one", member).await;
    let mut second = fixture.connect("room-two", member).await;
    let mut owner_socket = fixture.connect("room-two", owner).await;
    join(&mut first).await;
    join(&mut second).await;
    join(&mut owner_socket).await;

    lifecycle::remove_member(&fixture.state, "room-one", owner, member)
        .await
        .unwrap();

    expect_close(&mut first, 1008).await;
    second
        .send(update_frame(&update_with("key", "value")))
        .await
        .unwrap();
    assert!(matches!(
        next_message(&mut owner_socket).await,
        Message::Sync(SyncMessage::Update(_))
    ));
}

#[tokio::test]
async fn transferring_ownership_kicks_the_new_owner() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room("room-transfer", owner).await;
    let editor = fixture
        .member("room-transfer", "editor@example.org", Role::Editor)
        .await;
    let mut owner_socket = fixture.connect("room-transfer", owner).await;
    let mut editor_socket = fixture.connect("room-transfer", editor).await;
    join(&mut owner_socket).await;
    join(&mut editor_socket).await;

    lifecycle::transfer_ownership(&fixture.state, "room-transfer", owner, editor)
        .await
        .unwrap();

    expect_close(&mut editor_socket, 1008).await;
    owner_socket
        .send(update_frame(&update_with("key", "value")))
        .await
        .unwrap();
    assert!(matches!(
        next_message(&mut owner_socket).await,
        Message::Sync(SyncMessage::Update(_))
    ));
    let old = db::role_in(&fixture.state.pool, "room-transfer", owner)
        .await
        .unwrap();
    let new = db::role_in(&fixture.state.pool, "room-transfer", editor)
        .await
        .unwrap();
    assert_eq!(old, Some(Role::Editor));
    assert_eq!(new, Some(Role::Owner));
}

#[tokio::test]
async fn the_owner_role_is_not_assignable() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room("room-owner", owner).await;
    let editor = fixture
        .member("room-owner", "editor@example.org", Role::Editor)
        .await;

    let error =
        lifecycle::set_member_role(&fixture.state, "room-owner", owner, editor, Role::Owner)
            .await
            .unwrap_err();

    match error {
        LifecycleError::Conflict(reason) => assert!(reason.contains("owner keeps")),
        other => panic!("expected a conflict, got {other:?}"),
    }
}

#[tokio::test]
async fn the_owner_cannot_leave() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room("room-stay", owner).await;

    let error = lifecycle::remove_member(&fixture.state, "room-stay", owner, owner)
        .await
        .unwrap_err();

    match error {
        LifecycleError::Conflict(reason) => assert!(reason.contains("cannot leave")),
        other => panic!("expected a conflict, got {other:?}"),
    }
}

#[tokio::test]
async fn a_member_can_leave() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room("room-leave", owner).await;
    let viewer = fixture
        .member("room-leave", "viewer@example.org", Role::Viewer)
        .await;
    let mut socket = fixture.connect("room-leave", viewer).await;
    join(&mut socket).await;

    lifecycle::remove_member(&fixture.state, "room-leave", viewer, viewer)
        .await
        .unwrap();

    expect_close(&mut socket, 1008).await;
    let gone = db::role_in(&fixture.state.pool, "room-leave", viewer)
        .await
        .unwrap();
    assert_eq!(gone, None);
}
