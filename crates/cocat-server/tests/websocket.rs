use std::fs;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use cocat_core::role::Role;
use cocat_server::room::RoomManager;
use cocat_server::{AppState, auth, db, ws};
use futures_util::{SinkExt, StreamExt};
use sqlx::SqlitePool;
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
    pool: SqlitePool,
    _dir: TempDir,
    _server: JoinHandle<()>,
}

impl Fixture {
    async fn start() -> Self {
        let dir = TempDir::new().unwrap();
        let rooms_dir = dir.path().join("rooms");
        fs::create_dir_all(&rooms_dir).unwrap();
        let pool = db::open_writable(&dir.path().join("test.db"))
            .await
            .unwrap();
        db::ensure_schema(&pool).await.unwrap();
        let state = AppState {
            pool: pool.clone(),
            secret: SECRET.to_owned(),
            rooms: RoomManager::new(rooms_dir),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, ws::router(state)).await.unwrap();
        });
        Self {
            addr,
            pool,
            _dir: dir,
            _server: server,
        }
    }

    async fn user(&self, email: &str) -> Uuid {
        db::create_user(&self.pool, email, "hunter2", false)
            .await
            .unwrap()
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

    async fn connect(&self, room_id: &str, token: Option<&str>) -> Socket {
        self.handshake(room_id, token)
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

fn content(doc: &Doc, key: &str) -> Option<String> {
    doc.get_or_insert_map("content")
        .get(&doc.transact(), key)
        .map(|value| value.to_string(&doc.transact()))
}

#[tokio::test]
async fn an_anonymous_handshake_is_refused_with_403() {
    let fixture = Fixture::start().await;

    let status = fixture.handshake("room-anonymous", None).await.err();

    assert_eq!(status, Some(403));
}

#[tokio::test]
async fn a_non_member_handshake_is_refused_with_403() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    db::create_room(&fixture.pool, owner, "room-members", "Members")
        .await
        .unwrap();
    let outsider = fixture.user("outsider@example.org").await;

    let status = fixture
        .handshake("room-members", Some(&fixture.token(outsider)))
        .await
        .err();

    assert_eq!(status, Some(403));
}

#[tokio::test]
async fn a_member_receives_the_sync_step2() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    db::create_room(&fixture.pool, owner, "room-step", "Step")
        .await
        .unwrap();
    let mut socket = fixture
        .connect("room-step", Some(&fixture.token(owner)))
        .await;

    assert!(matches!(
        next_message(&mut socket).await,
        Message::Sync(SyncMessage::SyncStep1(_))
    ));

    socket.send(sync_frame(sync_step1())).await.unwrap();

    assert!(matches!(
        next_message(&mut socket).await,
        Message::Sync(SyncMessage::SyncStep2(_))
    ));
}

#[tokio::test]
async fn an_editor_update_reaches_another_socket() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let editor = fixture.user("editor@example.org").await;
    db::create_room(&fixture.pool, owner, "room-editors", "Editors")
        .await
        .unwrap();
    db::add_member(&fixture.pool, "room-editors", editor, Role::Editor)
        .await
        .unwrap();
    let mut first = fixture
        .connect("room-editors", Some(&fixture.token(owner)))
        .await;
    let mut second = fixture
        .connect("room-editors", Some(&fixture.token(editor)))
        .await;
    next_message(&mut first).await;
    next_message(&mut second).await;

    let payload = update_with("key", "value");
    first
        .send(sync_frame(Message::Sync(SyncMessage::Update(
            payload.clone(),
        ))))
        .await
        .unwrap();

    match next_message(&mut second).await {
        Message::Sync(SyncMessage::Update(update)) => assert_eq!(update, payload),
        other => panic!("expected an update, got {other:?}"),
    }
}

#[tokio::test]
async fn viewer_update_does_not_land() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let viewer = fixture.user("viewer@example.org").await;
    db::create_room(&fixture.pool, owner, "room-viewer", "Viewer")
        .await
        .unwrap();
    db::add_member(&fixture.pool, "room-viewer", viewer, Role::Viewer)
        .await
        .unwrap();
    let mut socket = fixture
        .connect("room-viewer", Some(&fixture.token(viewer)))
        .await;
    next_message(&mut socket).await;

    socket
        .send(sync_frame(Message::Sync(SyncMessage::Update(update_with(
            "key", "value",
        )))))
        .await
        .unwrap();
    socket.send(sync_frame(sync_step1())).await.unwrap();

    let reply = match next_message(&mut socket).await {
        Message::Sync(SyncMessage::SyncStep2(update)) => update,
        other => panic!("expected a step2, got {other:?}"),
    };
    let doc = Doc::new();
    doc.transact_mut()
        .apply_update(yrs::Update::decode_v1(&reply).unwrap())
        .unwrap();
    assert_eq!(content(&doc, "key"), None);
}

#[tokio::test]
async fn short_frames_do_not_close_the_socket() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    db::create_room(&fixture.pool, owner, "room-short", "Short")
        .await
        .unwrap();
    let mut socket = fixture
        .connect("room-short", Some(&fixture.token(owner)))
        .await;
    next_message(&mut socket).await;

    socket.send(WsMessage::Binary(Bytes::new())).await.unwrap();
    socket
        .send(WsMessage::Binary(Bytes::from_static(b"\x00")))
        .await
        .unwrap();
    socket
        .send(WsMessage::Text(String::new().into()))
        .await
        .unwrap();
    socket.send(sync_frame(sync_step1())).await.unwrap();

    assert!(matches!(
        next_message(&mut socket).await,
        Message::Sync(SyncMessage::SyncStep2(_))
    ));
}

#[tokio::test]
async fn an_accepted_socket_has_nagle_disabled() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connecting = tokio::spawn(async move { TcpStream::connect(addr).await });

    let (mut accepted, _) = listener.accept().await.unwrap();
    ws::disable_nagle(&mut accepted);
    assert!(accepted.nodelay().unwrap());
    connecting.await.unwrap().unwrap();
}
