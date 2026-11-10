use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async, connect_async_with_config,
};
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
            oidc: Vec::new(),
            web_dir: None,
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

    async fn token(&self, user: Uuid) -> String {
        auth::start_session(&self.pool, user).await.unwrap()
    }

    fn request(&self, room_id: &str, token: Option<&str>) -> Request<()> {
        let mut request = format!("ws://{}/room/{room_id}", self.addr)
            .into_client_request()
            .unwrap();
        if let Some(token) = token {
            request.headers_mut().insert(
                "cookie",
                format!("fastapiusersauth={token}").parse().unwrap(),
            );
        }
        request.headers_mut().insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            cocat_core::SUBPROTOCOL.parse().unwrap(),
        );
        request
    }

    async fn handshake(&self, room_id: &str, token: Option<&str>) -> Result<Socket, u16> {
        shake(self.request(room_id, token)).await
    }

    /// A handshake as Python cocat and the old SciQLop plugin make it: no subprotocol.
    async fn handshake_v1(&self, room_id: &str, token: &str) -> Result<Socket, u16> {
        let mut request = self.request(room_id, Some(token));
        request.headers_mut().remove(header::SEC_WEBSOCKET_PROTOCOL);
        shake(request).await
    }

    async fn connect(&self, room_id: &str, token: Option<&str>) -> Socket {
        self.handshake(room_id, token)
            .await
            .unwrap_or_else(|status| panic!("the handshake was refused with {status}"))
    }

    /// A client that may send frames above the server's limit, to prove the server refuses
    /// them rather than the client refusing to write them.
    async fn connect_wide(&self, room_id: &str, token: Option<&str>) -> Socket {
        let request = self.request(room_id, token);
        let wide = WebSocketConfig::default()
            .max_message_size(Some(64 << 20))
            .max_frame_size(Some(64 << 20));
        let (socket, response) = connect_async_with_config(request, Some(wide), true)
            .await
            .unwrap_or_else(|error| panic!("the handshake failed: {error}"));
        assert_eq!(response.status(), 101);
        socket
    }
}

async fn shake(request: Request<()>) -> Result<Socket, u16> {
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
async fn an_anonymous_handshake_is_refused_with_401() {
    let fixture = Fixture::start().await;

    let status = fixture.handshake("room-anonymous", None).await.err();

    assert_eq!(status, Some(401));
}

#[tokio::test]
async fn a_non_member_handshake_is_refused_with_403() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    db::create_room(
        &fixture.pool,
        owner,
        "room-members",
        "Members",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    let outsider = fixture.user("outsider@example.org").await;

    let status = fixture
        .handshake("room-members", Some(&fixture.token(outsider).await))
        .await
        .err();

    assert_eq!(status, Some(403));
}

#[tokio::test]
async fn a_member_receives_the_sync_step2() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    db::create_room(
        &fixture.pool,
        owner,
        "room-step",
        "Step",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    let mut socket = fixture
        .connect("room-step", Some(&fixture.token(owner).await))
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
async fn the_upgrade_answer_names_the_role() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let viewer = fixture.user("viewer@example.org").await;
    db::create_room(
        &fixture.pool,
        owner,
        "room-role",
        "Role",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    db::add_member(&fixture.pool, "room-role", viewer, Role::Viewer)
        .await
        .unwrap();

    for (user, role) in [(owner, "owner"), (viewer, "viewer")] {
        let request = fixture.request("room-role", Some(&fixture.token(user).await));
        let (_, response) = connect_async(request).await.unwrap();
        assert_eq!(
            response.headers()[cocat_core::ROLE_HEADER]
                .to_str()
                .unwrap(),
            role
        );
    }
}

#[tokio::test]
async fn an_editor_update_reaches_another_socket() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let editor = fixture.user("editor@example.org").await;
    db::create_room(
        &fixture.pool,
        owner,
        "room-editors",
        "Editors",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    db::add_member(&fixture.pool, "room-editors", editor, Role::Editor)
        .await
        .unwrap();
    let mut first = fixture
        .connect("room-editors", Some(&fixture.token(owner).await))
        .await;
    let mut second = fixture
        .connect("room-editors", Some(&fixture.token(editor).await))
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
    db::create_room(
        &fixture.pool,
        owner,
        "room-viewer",
        "Viewer",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    db::add_member(&fixture.pool, "room-viewer", viewer, Role::Viewer)
        .await
        .unwrap();
    let mut socket = fixture
        .connect("room-viewer", Some(&fixture.token(viewer).await))
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
    db::create_room(
        &fixture.pool,
        owner,
        "room-short",
        "Short",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    let mut socket = fixture
        .connect("room-short", Some(&fixture.token(owner).await))
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
    db::create_room(
        &fixture.pool,
        owner,
        "room-slow",
        "Slow",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    let token = fixture.token(owner).await;
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
    db::create_room(
        &fixture.pool,
        owner,
        "room-pinned",
        "Pinned",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    let (mut sink, _never_read) = fixture
        .connect("room-pinned", Some(&fixture.token(owner).await))
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
        db::create_room(&pool, owner, ROOM, "Downgrade", db::ProjectKind::Catalogues)
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
    let mut socket = fixture
        .connect(ROOM, Some(&fixture.token(editor).await))
        .await;

    // The handshake saw an editor; the role changed before the socket attached, so the
    // socket must not keep the editor it was granted.
    loop {
        match next_ws(&mut socket).await {
            WsMessage::Close(frame) => {
                assert_eq!(
                    frame.map(|frame| u16::from(frame.code)),
                    Some(4403),
                    "a socket whose role changed before the attach must close 4403"
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
    db::create_room(
        &fixture.pool,
        owner,
        "room-budget",
        "Budget",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    let (mut store, _) =
        cocat_core::store::FileRoomStore::open(&fixture.rooms_dir, "room-budget").unwrap();
    store
        .append(Bytes::from(update_with("key", "value")))
        .unwrap();
    store.flush().unwrap();

    let mut socket = fixture
        .connect("room-budget", Some(&fixture.token(owner).await))
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
    let token = fixture.token(owner).await;
    db::create_room(
        &fixture.pool,
        owner,
        "room-deleted",
        "Deleted",
        db::ProjectKind::Catalogues,
    )
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
            assert_eq!(frame.map(|frame| u16::from(frame.code)), Some(4403));
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
    db::create_room(
        &fixture.pool,
        owner,
        "room-grant",
        "Grant",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    db::add_member(&fixture.pool, "room-grant", guest, Role::Viewer)
        .await
        .unwrap();
    let state = fixture.state.clone();
    let (_, token) = invitations::create(&state, "room-grant", owner, Role::Editor, None, 14, None)
        .await
        .unwrap();
    let guest_token = fixture.token(guest).await;
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
            assert_eq!(frame.map(|frame| u16::from(frame.code)), Some(4403));
            break;
        }
    }
}

/// Deleting an account takes its socket with it before the row goes: a deleted account must
/// not keep writing, and its room must not stay loaded behind a socket nobody owns.
#[tokio::test]
async fn deleting_a_user_closes_their_sockets() {
    let fixture = Fixture::start().await;
    let victim = fixture.user("victim@example.org").await;
    let admin = db::create_user(&fixture.pool, "admin@example.org", "hunter2", true, true)
        .await
        .unwrap();
    db::create_room(
        &fixture.pool,
        victim,
        "room-a",
        "Room A",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    let mut socket = fixture
        .connect("room-a", Some(&fixture.token(victim).await))
        .await;
    next_ws(&mut socket).await;

    let request = Request::builder()
        .method(Method::DELETE)
        .uri(format!("/users/{victim}"))
        .header(
            header::COOKIE,
            format!("fastapiusersauth={}", fixture.token(admin).await),
        )
        .body(Body::empty())
        .unwrap();
    let status = ws::router(fixture.state.clone())
        .oneshot(request)
        .await
        .unwrap()
        .status();

    assert_eq!(status, StatusCode::NO_CONTENT);
    let close = loop {
        match next_ws(&mut socket).await {
            WsMessage::Close(frame) => break frame,
            _ => continue,
        }
    };
    assert_eq!(
        close.map(|frame| u16::from(frame.code)),
        Some(1008),
        "the deleted account's socket is revoked"
    );
    wait_for_unload(&fixture).await;
}

#[tokio::test]
async fn deleting_a_user_refuses_the_next_handshake() {
    let fixture = Fixture::start().await;
    let victim = fixture.user("victim@example.org").await;
    let victim_token = fixture.token(victim).await;
    let admin = db::create_user(&fixture.pool, "admin@example.org", "hunter2", true, true)
        .await
        .unwrap();
    db::create_room(
        &fixture.pool,
        victim,
        "room-a",
        "Room A",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();

    let request = Request::builder()
        .method(Method::DELETE)
        .uri(format!("/users/{victim}"))
        .header(
            header::COOKIE,
            format!("fastapiusersauth={}", fixture.token(admin).await),
        )
        .body(Body::empty())
        .unwrap();
    let status = ws::router(fixture.state.clone())
        .oneshot(request)
        .await
        .unwrap()
        .status();

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        db::user_by_id(&fixture.pool, victim)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fixture.handshake("room-a", Some(&victim_token)).await.err(),
        Some(401),
        "a deleted account's session is gone: sign in again"
    );
}

#[tokio::test]
async fn deactivating_a_user_closes_their_sockets() {
    let fixture = Fixture::start().await;
    let victim = fixture.user("victim@example.org").await;
    let admin = db::create_user(&fixture.pool, "admin@example.org", "hunter2", true, true)
        .await
        .unwrap();
    db::create_room(
        &fixture.pool,
        victim,
        "room-a",
        "Room A",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    let mut socket = fixture
        .connect("room-a", Some(&fixture.token(victim).await))
        .await;
    next_ws(&mut socket).await;

    let request = Request::builder()
        .method(Method::PATCH)
        .uri(format!("/users/{victim}"))
        .header(
            header::COOKIE,
            format!("fastapiusersauth={}", fixture.token(admin).await),
        )
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"is_active": false}"#))
        .unwrap();
    let status = ws::router(fixture.state.clone())
        .oneshot(request)
        .await
        .unwrap()
        .status();

    assert_eq!(status, StatusCode::OK);
    let close = loop {
        match next_ws(&mut socket).await {
            WsMessage::Close(frame) => break frame,
            _ => continue,
        }
    };
    assert_eq!(
        close.map(|frame| u16::from(frame.code)),
        Some(1008),
        "a deactivated account is cut off at once, not at its next reconnect"
    );
    assert_eq!(
        fixture
            .handshake("room-a", Some(&fixture.token(victim).await))
            .await
            .err(),
        Some(401),
        "the deactivated account cannot reconnect"
    );
    wait_for_unload(&fixture).await;
}

#[tokio::test]
async fn sixteen_mib_update_is_accepted() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    db::create_room(
        &fixture.pool,
        owner,
        "room-16mib",
        "Sixteen",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    let mut socket = fixture
        .connect("room-16mib", Some(&fixture.token(owner).await))
        .await;
    next_message(&mut socket).await;

    let payload = update_with("key", &"a".repeat(ws::MAX_MESSAGE_BYTES - 1024));
    assert!(
        payload.len() <= ws::MAX_MESSAGE_BYTES,
        "the update itself fits in one message"
    );
    socket
        .send(sync_frame(Message::Sync(SyncMessage::Update(
            payload.clone(),
        ))))
        .await
        .unwrap();

    match next_message(&mut socket).await {
        Message::Sync(SyncMessage::Update(update)) => assert_eq!(update, payload),
        other => panic!("expected the broadcast update, got {other:?}"),
    }
}

#[tokio::test]
async fn oversized_message_closes_the_socket() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    db::create_room(
        &fixture.pool,
        owner,
        "room-too-big",
        "TooBig",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    let mut socket = fixture
        .connect_wide("room-too-big", Some(&fixture.token(owner).await))
        .await;
    next_message(&mut socket).await;

    // A frame above the limit is refused at its header, so a client still writing it is
    // reset before the close frame can reach it; a client that finished writing reads the
    // 1009 the bridge sends, which is also how a message that grows past the limit through
    // fragments ends. Either way the message never lands.
    if socket
        .send(WsMessage::Binary(Bytes::from(vec![
            0u8;
            ws::MAX_MESSAGE_BYTES
                + 1
        ])))
        .await
        .is_err()
    {
        return;
    }
    loop {
        match timeout(WAIT, socket.next())
            .await
            .expect("the socket went quiet")
        {
            Some(Ok(WsMessage::Close(frame))) => {
                assert_eq!(
                    frame.map(|frame| u16::from(frame.code)),
                    Some(1009),
                    "an oversized message must close the socket with 1009"
                );
                break;
            }
            Some(Ok(_)) => {}
            Some(Err(error)) => panic!("the socket dropped without a close frame: {error}"),
            None => panic!("the socket dropped without a close frame"),
        }
    }
}

#[tokio::test]
async fn a_handshake_without_the_v2_subprotocol_is_refused_with_426() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    db::create_room(
        &fixture.pool,
        owner,
        "room-v1",
        "Old client",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    let file = fixture.rooms_dir.join("room-v1.y");
    let (mut store, _) =
        cocat_core::store::FileRoomStore::open(&fixture.rooms_dir, "room-v1").unwrap();
    store
        .append(Bytes::from(update_with("kept", "as is")))
        .unwrap();
    store.flush().unwrap();
    drop(store);
    let before = fs::read(&file).unwrap();

    let status = fixture
        .handshake_v1("room-v1", &fixture.token(owner).await)
        .await
        .err();

    assert_eq!(status, Some(426));
    assert_eq!(fs::read(&file).unwrap(), before);
    assert_eq!(fixture.rooms.open_rooms(), 0);
}

#[tokio::test]
async fn an_accepted_handshake_names_the_v2_subprotocol() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    db::create_room(
        &fixture.pool,
        owner,
        "room-v2",
        "New client",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    let request = fixture.request("room-v2", Some(&fixture.token(owner).await));

    let (_socket, response) = connect_async(request).await.unwrap();

    assert_eq!(
        response.headers().get(header::SEC_WEBSOCKET_PROTOCOL),
        Some(&header::HeaderValue::from_static(cocat_core::SUBPROTOCOL))
    );
}

/// A room Python cocat wrote, in layout v1.
const V1_ROOM: &[u8] = include_bytes!("../../cocat-core/tests/fixtures/catalogues.y");

#[tokio::test]
async fn a_room_file_of_the_first_layout_is_never_served() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    db::create_room(
        &fixture.pool,
        owner,
        "room-old",
        "Not imported",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    let file = fixture.rooms_dir.join("room-old.y");
    fs::write(&file, V1_ROOM).unwrap();

    let mut socket = fixture
        .connect("room-old", Some(&fixture.token(owner).await))
        .await;

    match next_ws(&mut socket).await {
        WsMessage::Close(frame) => assert_eq!(frame.map(|frame| u16::from(frame.code)), Some(1011)),
        other => panic!("a v1 room must not be served, got {other:?}"),
    }
    assert_eq!(fs::read(&file).unwrap(), V1_ROOM);
    assert_eq!(fixture.rooms.open_rooms(), 0);
}

#[tokio::test]
async fn a_socket_relays_awareness_under_the_account_email() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    db::create_room(
        &fixture.pool,
        owner,
        "room-presence",
        "Presence",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    let viewer = fixture.user("viewer@example.org").await;
    db::add_member(&fixture.pool, "room-presence", viewer, Role::Viewer)
        .await
        .unwrap();
    let mut first = fixture
        .connect("room-presence", Some(&fixture.token(owner).await))
        .await;
    let mut second = fixture
        .connect("room-presence", Some(&fixture.token(viewer).await))
        .await;
    next_message(&mut first).await;
    next_message(&mut second).await;

    let claim = yrs::sync::AwarenessUpdate {
        clients: std::collections::HashMap::from([(
            yrs::ClientID::new(42),
            yrs::sync::awareness::AwarenessUpdateEntry {
                clock: 1,
                json: Arc::from(r#"{"user":{"email":"owner@example.org"}}"#),
            },
        )]),
    };
    second
        .send(sync_frame(Message::Awareness(claim)))
        .await
        .unwrap();

    let Message::Awareness(seen) = next_message(&mut first).await else {
        panic!("expected an awareness update");
    };
    let state: serde_json::Value =
        serde_json::from_str(&seen.clients[&yrs::ClientID::new(42)].json).unwrap();
    assert_eq!(state["user"]["email"], "viewer@example.org");
}

fn bearer_request(fixture: &Fixture, room_id: &str, token: &str) -> Request<()> {
    let mut request = fixture.request(room_id, None);
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    request
}

async fn close_code(socket: &mut Socket) -> Option<u16> {
    loop {
        if let WsMessage::Close(frame) = next_ws(socket).await {
            return frame.map(|frame| u16::from(frame.code));
        }
    }
}

#[tokio::test]
async fn a_bearer_token_opens_a_socket() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    db::create_room(
        &fixture.pool,
        owner,
        "room-bearer",
        "Bearer",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    let token = auth::start_token(&fixture.pool, owner, "Python on laptop")
        .await
        .unwrap();

    let mut socket = shake(bearer_request(&fixture, "room-bearer", &token))
        .await
        .expect("the bearer token is accepted");
    next_message(&mut socket).await;

    assert_eq!(
        shake(bearer_request(&fixture, "room-bearer", "not-a-token"))
            .await
            .err(),
        Some(401)
    );
}

#[tokio::test]
async fn revoking_a_token_closes_its_socket_at_once() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    db::create_room(
        &fixture.pool,
        owner,
        "room-revoke",
        "Revoke",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    let token = auth::start_token(&fixture.pool, owner, "Python on laptop")
        .await
        .unwrap();
    let browser = fixture.token(owner).await;
    let mut app = shake(bearer_request(&fixture, "room-revoke", &token))
        .await
        .unwrap();
    next_message(&mut app).await;
    let mut tab = fixture.connect("room-revoke", Some(&browser)).await;
    next_message(&mut tab).await;

    let request = Request::builder()
        .method(Method::DELETE)
        .uri("/api/tokens/current")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = ws::router(fixture.state.clone())
        .oneshot(request)
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(close_code(&mut app).await, Some(1008));
    wait_for_sockets(&fixture, 1).await;
    drop(tab);
}

/// The room detaches a closed socket after the close frame went out, so the count is polled
/// with a deadline rather than read once.
async fn wait_for_sockets(fixture: &Fixture, expected: usize) {
    let deadline = Instant::now() + WAIT;
    loop {
        let open = fixture.rooms.sockets().await.len();
        if open == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{open} sockets, expected {expected}"
        );
        sleep(Duration::from_millis(10)).await;
    }
}
