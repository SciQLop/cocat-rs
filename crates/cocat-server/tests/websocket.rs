use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use bytes::Bytes;
use cocat_core::role::Role;
use cocat_server::room::{OUTBOUND_FRAMES, RoomManager};
use cocat_server::{
    AppState, BeforeAttach, auth, db, invitations, links::Links, ratelimit::LoginLimit, ws,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tower::ServiceExt;
use uuid::Uuid;
use yrs::sync::{Message, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, Map, ReadTxn, StateVector, Transact};

const SECRET: &str = "test-secret";
const WAIT: Duration = Duration::from_secs(10);

/// A slow client only overflows once the kernel has swallowed several megabytes, so the
/// updates carry 4 KB values: `OUTBOUND_FRAMES` of them plus what the socket buffers hold.
const SLOW_PADDING: usize = 4096;
const SLOW_UPDATES: usize = OUTBOUND_FRAMES + 4096;

/// 16 KB frames fill a loopback send buffer in a few hundred frames, leaving the 1024-frame
/// queue as the bound that drops the client.
const PIN_PADDING: usize = 16 * 1024;
const PIN_UPDATES: usize = OUTBOUND_FRAMES + 1024;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Fixture {
    addr: SocketAddr,
    state: AppState,
    pool: SqlitePool,
    rooms: RoomManager,
    rooms_dir: PathBuf,
    _dir: TempDir,
    _server: JoinHandle<()>,
}

impl Fixture {
    async fn start() -> Self {
        Self::build(|_: SqlitePool| async { None }, None).await
    }

    /// `prepare` runs on the fixture's pool before the server answers anything, so a test
    /// can seed data and hand back a hook that acts between the handshake and the attach.
    async fn start_with<F, Fut>(prepare: F) -> Self
    where
        F: FnOnce(SqlitePool) -> Fut,
        Fut: std::future::Future<Output = Option<BeforeAttach>>,
    {
        Self::build(prepare, None).await
    }

    async fn start_with_budget(budget: u64) -> Self {
        Self::build(|_: SqlitePool| async { None }, Some(budget)).await
    }

    async fn build<F, Fut>(prepare: F, budget: Option<u64>) -> Self
    where
        F: FnOnce(SqlitePool) -> Fut,
        Fut: std::future::Future<Output = Option<BeforeAttach>>,
    {
        let dir = TempDir::new().unwrap();
        let rooms_dir = dir.path().join("rooms");
        fs::create_dir_all(&rooms_dir).unwrap();
        let pool = db::open(&dir.path().join("test.db")).await.unwrap();
        let before_attach = prepare(pool.clone()).await;
        let rooms = RoomManager::new(rooms_dir.clone(), budget);
        let state = AppState {
            pool: pool.clone(),
            secret: SECRET.to_owned(),
            rooms: rooms.clone(),
            before_attach,
            login_limit: LoginLimit::default(),
            mailer: None,
            links: Links::default(),
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
            pool,
            rooms,
            rooms_dir,
            _dir: dir,
            _server: server,
        }
    }

    async fn user(&self, email: &str) -> Uuid {
        db::create_user(&self.pool, email, "hunter2", false, true)
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
async fn a_client_that_does_not_read_is_closed_1013() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    db::create_room(&fixture.pool, owner, "room-slow", "Slow")
        .await
        .unwrap();
    let token = fixture.token(owner);
    let mut silent = fixture.connect("room-slow", Some(&token)).await;
    let mut writer = fixture.connect("room-slow", Some(&token)).await;
    next_message(&mut writer).await;

    let padding = "x".repeat(SLOW_PADDING);
    for tick in 0..SLOW_UPDATES {
        let update = update_with(&format!("key-{tick}"), &padding);
        writer
            .send(sync_frame(Message::Sync(SyncMessage::Update(update))))
            .await
            .unwrap();
        // Awaiting its own broadcast keeps the writer's queue at one frame, so only the
        // silent socket's queue fills.
        next_message(&mut writer).await;
    }

    // The room dropped the silent client while its socket was blocked on the kernel
    // buffers; reading it now lets the queued frames and then the close through.
    loop {
        if let WsMessage::Close(frame) = next_ws(&mut silent).await {
            assert_eq!(
                frame.map(|frame| u16::from(frame.code)),
                Some(1013),
                "a client that does not read must be closed 1013"
            );
            break;
        }
    }
}

#[tokio::test]
async fn a_client_that_stops_reading_does_not_pin_the_room() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    db::create_room(&fixture.pool, owner, "room-pinned", "Pinned")
        .await
        .unwrap();
    let (mut sink, _never_read) = fixture
        .connect("room-pinned", Some(&fixture.token(owner)))
        .await
        .split();

    // The only client floods its own room and never reads a byte back: its outbox fills,
    // its writer blocks on a socket the kernel will not accept more bytes for, and the
    // bridge has no reason left to live. A single client is enough, because the race is
    // between the writer and the read loop, not between two clients.
    let padding = "x".repeat(PIN_PADDING);
    for tick in 0..PIN_UPDATES {
        let update = update_with(&format!("key-{tick}"), &padding);
        sink.send(sync_frame(Message::Sync(SyncMessage::Update(update))))
            .await
            .unwrap();
    }

    wait_for_unload(&fixture).await;
}

/// The room registry has no event to await when a room leaves it, so poll it; the bounded
/// deadline still fails loudly when a stuck bridge pins its room.
async fn wait_for_unload(fixture: &Fixture) {
    let deadline = Instant::now() + WAIT + ws::WRITE_TIMEOUT;
    while fixture.rooms.open_rooms() != 0 {
        assert!(
            Instant::now() < deadline,
            "the client that stopped reading still pins its room"
        );
        sleep(Duration::from_millis(25)).await;
    }
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

#[tokio::test]
async fn a_downgrade_between_handshake_and_attach_is_honoured() {
    const ROOM: &str = "room-downgrade";
    let fixture = Fixture::start_with(|pool| async move {
        let owner = db::create_user(&pool, "owner@example.org", "hunter2", false, true)
            .await
            .unwrap();
        db::create_room(&pool, owner, ROOM, "Downgrade")
            .await
            .unwrap();
        let editor = db::create_user(&pool, "editor@example.org", "hunter2", false, true)
            .await
            .unwrap();
        db::add_member(&pool, ROOM, editor, Role::Editor)
            .await
            .unwrap();
        let hook: BeforeAttach = Arc::new(move || {
            let pool = pool.clone();
            Box::pin(async move {
                db::set_role(&pool, ROOM, editor, Role::Viewer)
                    .await
                    .expect("the downgrade lands");
            })
        });
        Some(hook)
    })
    .await;

    let editor = db::user_by_email(&fixture.pool, "editor@example.org")
        .await
        .unwrap()
        .unwrap()
        .id;
    let mut socket = fixture.connect(ROOM, Some(&fixture.token(editor))).await;

    // The handshake saw an editor; the role changed before the socket attached, so the
    // socket must not keep the editor it was granted.
    loop {
        match next_ws(&mut socket).await {
            WsMessage::Close(frame) => {
                assert_eq!(
                    frame.map(|frame| u16::from(frame.code)),
                    Some(1008),
                    "a socket whose role changed before the attach must close 1008"
                );
                break;
            }
            WsMessage::Binary(_) => {}
            other => panic!("expected a close, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn an_over_budget_room_closes_1013() {
    // One byte of budget: any room with a file on disk is over it.
    let fixture = Fixture::start_with_budget(1).await;
    let owner = fixture.user("owner@example.org").await;
    db::create_room(&fixture.pool, owner, "room-budget", "Budget")
        .await
        .unwrap();
    let (mut store, _) =
        cocat_core::store::FileRoomStore::open(&fixture.rooms_dir, "room-budget").unwrap();
    store
        .append(Bytes::from(update_with("key", "value")))
        .unwrap();
    store.flush().unwrap();

    let mut socket = fixture
        .connect("room-budget", Some(&fixture.token(owner)))
        .await;

    loop {
        match next_ws(&mut socket).await {
            WsMessage::Close(frame) => {
                assert_eq!(
                    frame.map(|frame| u16::from(frame.code)),
                    Some(1013),
                    "an over-budget room must close 1013"
                );
                break;
            }
            WsMessage::Binary(_) => {}
            other => panic!("expected a close, got {other:?}"),
        }
    }
    assert_eq!(fixture.rooms.open_rooms(), 0);
}

/// The live socket half needs a real listener, so this test lives with the websocket
/// fixture; the DELETE goes through the same router the server runs, in process.
#[tokio::test]
async fn deleting_over_http_closes_the_sockets_and_the_file() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let token = fixture.token(owner);
    db::create_room(&fixture.pool, owner, "room-deleted", "Deleted")
        .await
        .unwrap();
    let mut socket = fixture.connect("room-deleted", Some(&token)).await;
    next_message(&mut socket).await;
    socket
        .send(sync_frame(Message::Sync(SyncMessage::Update(update_with(
            "key", "value",
        )))))
        .await
        .unwrap();
    match next_message(&mut socket).await {
        Message::Sync(SyncMessage::Update(_)) => {}
        other => panic!("expected the broadcast update, got {other:?}"),
    }
    let file = fixture.rooms_dir.join("room-deleted.y");
    assert!(file.exists(), "the update was never written");

    let request = Request::builder()
        .method(Method::DELETE)
        .uri("/api/rooms/room-deleted")
        .header(header::COOKIE, format!("fastapiusersauth={token}"))
        .body(Body::empty())
        .unwrap();
    let response = ws::router(fixture.state.clone())
        .oneshot(request)
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    loop {
        if let WsMessage::Close(frame) = next_ws(&mut socket).await {
            assert_eq!(frame.map(|frame| u16::from(frame.code)), Some(1008));
            break;
        }
    }
    assert!(!file.exists(), "the deleted room left its file behind");
}

#[tokio::test]
async fn accepting_a_higher_invitation_closes_the_old_socket() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let guest = fixture.user("guest@example.org").await;
    db::create_room(&fixture.pool, owner, "room-grant", "Grant")
        .await
        .unwrap();
    db::add_member(&fixture.pool, "room-grant", guest, Role::Viewer)
        .await
        .unwrap();
    let state = fixture.state.clone();
    let (_, token) = invitations::create(&state, "room-grant", owner, Role::Editor, None, 14, None)
        .await
        .unwrap();
    let guest_token = fixture.token(guest);
    let mut socket = fixture.connect("room-grant", Some(&guest_token)).await;
    next_message(&mut socket).await;

    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/invitations/accept")
        .header(header::COOKIE, format!("fastapiusersauth={guest_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"token": token}).to_string()))
        .unwrap();
    let response = ws::router(state).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    loop {
        if let WsMessage::Close(frame) = next_ws(&mut socket).await {
            assert_eq!(frame.map(|frame| u16::from(frame.code)), Some(1008));
            break;
        }
    }
}
