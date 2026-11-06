//! Group access over real sockets: the effective role a grant gives, and the re-read that
//! takes it away when a member leaves the group between the handshake and the attach.

use std::fs;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use cocat_server::room::RoomManager;
use cocat_server::{AppState, BeforeAttach, auth, db, links::Links, ratelimit::LoginLimit, ws};
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
const ROOM: &str = "room-group";

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Fixture {
    addr: SocketAddr,
    pool: SqlitePool,
    _dir: TempDir,
    _server: JoinHandle<()>,
}

impl Fixture {
    /// `prepare` seeds the database before the server answers, and can hand back a hook that
    /// acts between the handshake and the attach.
    async fn start_with<F, Fut>(prepare: F) -> Self
    where
        F: FnOnce(SqlitePool) -> Fut,
        Fut: std::future::Future<Output = Option<BeforeAttach>>,
    {
        Self::build(prepare).await
    }

    async fn build<F, Fut>(prepare: F) -> Self
    where
        F: FnOnce(SqlitePool) -> Fut,
        Fut: std::future::Future<Output = Option<BeforeAttach>>,
    {
        let dir = TempDir::new().unwrap();
        let rooms_dir = dir.path().join("rooms");
        fs::create_dir_all(&rooms_dir).unwrap();
        let pool = db::open(&dir.path().join("test.db")).await.unwrap();
        let before_attach = prepare(pool.clone()).await;
        let state = AppState {
            pool: pool.clone(),
            secret: SECRET.to_owned(),
            rooms: RoomManager::new(rooms_dir, None),
            before_attach,
            login_limit: LoginLimit::default(),
            mailer: None,
            links: Links::default(),
            oidc: Vec::new(),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let served = state.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, ws::router(served)).await.unwrap();
        });
        Self {
            addr,
            pool,
            _dir: dir,
            _server: server,
        }
    }

    fn token(&self, user: Uuid) -> String {
        auth::mint_token(user, SECRET, SystemTime::now()).unwrap()
    }

    async fn connect(&self, room_id: &str, token: &str) -> Socket {
        let mut request = format!("ws://{}/room/{room_id}", self.addr)
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "cookie",
            format!("fastapiusersauth={token}").parse().unwrap(),
        );
        let (socket, response) = connect_async(request).await.expect("the handshake failed");
        assert_eq!(response.status(), 101);
        socket
    }
}

/// A room owned by `owner`, shared with a group whose members are `members`, at `role`.
async fn seed(pool: &SqlitePool, owner: Uuid, members: &[Uuid], role: &str) -> String {
    db::create_room(pool, owner, ROOM, "Group room")
        .await
        .unwrap();
    let group_id = Uuid::new_v4().simple().to_string();
    sqlx::query("INSERT INTO user_group (id, name, created_by) VALUES (?, 'Team', ?)")
        .bind(&group_id)
        .bind(owner.to_string())
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO group_member (group_id, user_id, role) VALUES (?, ?, 'owner')")
        .bind(&group_id)
        .bind(owner.to_string())
        .execute(pool)
        .await
        .unwrap();
    for member in members {
        sqlx::query("INSERT INTO group_member (group_id, user_id, role) VALUES (?, ?, 'member')")
            .bind(&group_id)
            .bind(member.to_string())
            .execute(pool)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO room_group (room_id, group_id, role) VALUES (?, ?, ?)")
        .bind(ROOM)
        .bind(&group_id)
        .bind(role)
        .execute(pool)
        .await
        .unwrap();
    group_id
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
async fn group_member_syncs_the_room() {
    let fixture = Fixture::start_with(|pool| async move {
        let owner = db::create_user(&pool, "owner@example.org", "hunter2", false, true)
            .await
            .unwrap();
        let editor = db::create_user(&pool, "editor@example.org", "hunter2", false, true)
            .await
            .unwrap();
        seed(&pool, owner, &[editor], "editor").await;
        None
    })
    .await;
    let owner = db::user_by_email(&fixture.pool, "owner@example.org")
        .await
        .unwrap()
        .unwrap()
        .id;
    let editor = db::user_by_email(&fixture.pool, "editor@example.org")
        .await
        .unwrap()
        .unwrap()
        .id;

    let mut direct = fixture.connect(ROOM, &fixture.token(owner)).await;
    let mut granted = fixture.connect(ROOM, &fixture.token(editor)).await;
    next_message(&mut direct).await;
    next_message(&mut granted).await;

    let payload = update_with("key", "value");
    granted
        .send(sync_frame(Message::Sync(SyncMessage::Update(
            payload.clone(),
        ))))
        .await
        .unwrap();

    match next_message(&mut direct).await {
        Message::Sync(SyncMessage::Update(update)) => assert_eq!(update, payload),
        other => panic!("expected the group editor's update, got {other:?}"),
    }
}

#[tokio::test]
async fn group_viewer_writes_are_dropped() {
    let fixture = Fixture::start_with(|pool| async move {
        let owner = db::create_user(&pool, "owner@example.org", "hunter2", false, true)
            .await
            .unwrap();
        let viewer = db::create_user(&pool, "viewer@example.org", "hunter2", false, true)
            .await
            .unwrap();
        seed(&pool, owner, &[viewer], "viewer").await;
        None
    })
    .await;
    let viewer = db::user_by_email(&fixture.pool, "viewer@example.org")
        .await
        .unwrap()
        .unwrap()
        .id;

    let mut socket = fixture.connect(ROOM, &fixture.token(viewer)).await;
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
async fn removed_from_group_mid_handshake_is_closed() {
    let fixture = Fixture::start_with(|pool| async move {
        let owner = db::create_user(&pool, "owner@example.org", "hunter2", false, true)
            .await
            .unwrap();
        let member = db::create_user(&pool, "member@example.org", "hunter2", false, true)
            .await
            .unwrap();
        let group_id = seed(&pool, owner, &[member], "editor").await;
        let hook: BeforeAttach = Arc::new(move || {
            let pool = pool.clone();
            let group_id = group_id.clone();
            Box::pin(async move {
                sqlx::query("DELETE FROM group_member WHERE group_id = ? AND user_id = ?")
                    .bind(&group_id)
                    .bind(member.to_string())
                    .execute(&pool)
                    .await
                    .expect("the removal lands");
            })
        });
        Some(hook)
    })
    .await;
    let member = db::user_by_email(&fixture.pool, "member@example.org")
        .await
        .unwrap()
        .unwrap()
        .id;

    let mut socket = fixture.connect(ROOM, &fixture.token(member)).await;

    // The handshake saw the grant; the membership went before the attach, so the post-attach
    // re-read finds no role and the socket must not keep the editor it was granted.
    loop {
        match next_ws(&mut socket).await {
            WsMessage::Close(frame) => {
                assert_eq!(
                    frame.map(|frame| u16::from(frame.code)),
                    Some(1008),
                    "a socket whose group membership vanished before the attach must close 1008"
                );
                break;
            }
            WsMessage::Binary(_) => {}
            other => panic!("expected a close, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_former_member_cannot_reconnect() {
    let fixture = Fixture::start_with(|pool| async move {
        let owner = db::create_user(&pool, "owner@example.org", "hunter2", false, true)
            .await
            .unwrap();
        let member = db::create_user(&pool, "member@example.org", "hunter2", false, true)
            .await
            .unwrap();
        seed(&pool, owner, &[member], "editor").await;
        None
    })
    .await;
    let member = db::user_by_email(&fixture.pool, "member@example.org")
        .await
        .unwrap()
        .unwrap()
        .id;

    let mut request = format!("ws://{}/room/{ROOM}", fixture.addr)
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "cookie",
        format!("fastapiusersauth={}", fixture.token(member))
            .parse()
            .unwrap(),
    );
    // Drop the grant, then the membership: the room is no longer reachable.
    sqlx::query("DELETE FROM room_group WHERE room_id = ?")
        .bind(ROOM)
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM group_member WHERE user_id = ?")
        .bind(member.to_string())
        .execute(&fixture.pool)
        .await
        .unwrap();

    let refused = match connect_async(request).await {
        Ok(_) => panic!("a former member must not reach the room"),
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => response.status().as_u16(),
        Err(error) => panic!("the handshake failed: {error}"),
    };
    assert_eq!(refused, 403);
}
