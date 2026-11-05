use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use cocat_core::store::FileRoomStore;
use cocat_server::{auth, db};
use futures_util::{SinkExt, StreamExt};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use uuid::Uuid;
use yrs::sync::{Message, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, Map, ReadTxn, StateVector, Transact};

const WAIT: Duration = Duration::from_secs(10);
const ROOM_ID: &str = "serve-room";

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Server {
    child: Child,
    addr: SocketAddr,
    update_dir: PathBuf,
    secret: String,
    owner: Uuid,
    _dir: TempDir,
}

impl Server {
    async fn spawn() -> Self {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let update_dir = dir.path().join("rooms");

        let pool = db::open(&db_path).await.unwrap();
        let owner = db::create_user(&pool, "owner@example.org", "hunter2", false)
            .await
            .unwrap();
        db::create_room(&pool, owner, ROOM_ID, "Serve")
            .await
            .unwrap();
        pool.close().await;
        let secret = auth::load_secret(&db_path).unwrap();

        let mut child = Command::new(env!("CARGO_BIN_EXE_cocat-server"))
            .arg("serve")
            .args(["--host", "127.0.0.1", "--port", "0"])
            .arg("--update_dir")
            .arg(&update_dir)
            .arg("--db_path")
            .arg(&db_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut lines = BufReader::new(stdout).lines();
        let line = timeout(WAIT, lines.next_line())
            .await
            .expect("the server printed nothing")
            .expect("the server closed stdout")
            .expect("the server printed no line");
        let addr = line
            .strip_prefix("listening on ")
            .unwrap_or_else(|| panic!("unexpected first line: {line}"))
            .parse()
            .unwrap();
        Self {
            child,
            addr,
            update_dir,
            secret,
            owner,
            _dir: dir,
        }
    }

    async fn signal(&mut self, signal: &str) {
        let pid = self.child.id().unwrap();
        let killed = Command::new("kill")
            .args([signal, &pid.to_string()])
            .status()
            .await
            .unwrap();
        assert!(killed.success());
        let status = timeout(WAIT, self.child.wait())
            .await
            .unwrap_or_else(|_| panic!("the server ignored {signal}"))
            .unwrap();
        assert_eq!(status.code(), Some(0));
    }

    async fn interrupt(&mut self) {
        self.signal("-INT").await;
    }
}

fn update_with(key: &str, value: &str) -> Vec<u8> {
    let doc = Doc::new();
    doc.get_or_insert_map("content")
        .insert(&mut doc.transact_mut(), key, value);
    doc.transact()
        .encode_state_as_update_v1(&StateVector::default())
}

async fn connect(server: &Server, token: &str) -> Socket {
    let mut request = format!("ws://{}/room/{ROOM_ID}", server.addr)
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "cookie",
        format!("fastapiusersauth={token}").parse().unwrap(),
    );
    let (socket, _) = connect_async(request).await.unwrap();
    socket
}

async fn next_message(socket: &mut Socket) -> Message {
    let frame = timeout(WAIT, socket.next())
        .await
        .expect("the socket went quiet")
        .expect("the socket failed")
        .expect("the socket closed");
    match frame {
        WsMessage::Binary(bytes) => {
            Message::decode_v1(&bytes).expect("the server sent a bad frame")
        }
        other => panic!("expected a binary frame, got {other:?}"),
    }
}

#[tokio::test]
async fn serve_prints_the_bound_port() {
    let mut server = Server::spawn().await;

    assert_ne!(server.addr.port(), 0);
    assert!(server.child.try_wait().unwrap().is_none());

    server.interrupt().await;
}

#[tokio::test]
async fn the_root_answers_404() {
    let mut server = Server::spawn().await;
    let mut stream = TcpStream::connect(server.addr).await.unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();

    assert!(response.starts_with("HTTP/1.1 404"), "{response}");

    server.interrupt().await;
}

async fn the_room_survives_a_signal(signal: &str) {
    let mut server = Server::spawn().await;
    let token = auth::mint_token(server.owner, &server.secret, SystemTime::now()).unwrap();
    let mut socket = connect(&server, &token).await;
    next_message(&mut socket).await;

    let payload = update_with("key", "value");
    socket
        .send(WsMessage::Binary(Bytes::from(
            Message::Sync(SyncMessage::SyncStep2(payload.clone())).encode_v1(),
        )))
        .await
        .unwrap();
    match next_message(&mut socket).await {
        Message::Sync(SyncMessage::Update(update)) if update == payload => {}
        other => panic!("expected an update, got {other:?}"),
    }

    server.signal(signal).await;

    let (_, updates) = FileRoomStore::open(&server.update_dir, ROOM_ID).unwrap();
    assert_eq!(updates, [Bytes::from(payload)]);
}

#[tokio::test]
async fn sigint_flushes_and_exits_zero() {
    the_room_survives_a_signal("-INT").await;
}

#[tokio::test]
async fn sigterm_flushes_and_exits_zero() {
    the_room_survives_a_signal("-TERM").await;
}
