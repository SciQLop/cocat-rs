//! A cocat-server on a free port, seeded the way the server's own tests seed one, plus the
//! few document helpers the client tests write through.

#![allow(dead_code)]

use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use cocat_client::{Change, Room};
use cocat_core::model::{self, EventRecord, NewCatalogue};
use cocat_core::role::Role;
use cocat_core::time::Timestamp;
use cocat_server::room::RoomManager;
use cocat_server::{AppState, auth, db, links::Links, ratelimit::LoginLimit, ws};
use futures_util::{SinkExt, StreamExt};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message as Frame;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use uuid::Uuid;
use yrs::StateVector;
use yrs::Transact;
use yrs::sync::{Message, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;

pub const PASSWORD: &str = "hunter2";
pub const WAIT: Duration = Duration::from_secs(15);

/// A cocat server on its own runtime and thread, so stopping it is a real restart: the
/// runtime drops every connection it holds, and the client sees the socket go away the way
/// it would when the process dies.
pub struct Server {
    pub addr: SocketAddr,
    pub pool: SqlitePool,
    /// How many times a client posted to the login route, so a test can see a retry loop.
    pub logins: Arc<AtomicUsize>,
    dir: TempDir,
    rooms_dir: PathBuf,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    pub async fn start() -> Server {
        let dir = TempDir::new().unwrap();
        let rooms_dir = dir.path().join("rooms");
        fs::create_dir_all(&rooms_dir).unwrap();
        let pool = db::open(&dir.path().join("cocat.db")).await.unwrap();
        let mut server = Self {
            addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            pool,
            logins: Arc::new(AtomicUsize::new(0)),
            dir,
            rooms_dir,
            stop: None,
            thread: None,
        };
        server.serve(SocketAddr::from(([127, 0, 0, 1], 0)));
        server
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Stops the server, waiting until its port and its sockets are gone.
    pub async fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread.join().expect("the server thread stopped cleanly");
        }
    }

    /// The same database and room files on the same port, as after a real restart.
    pub async fn restart(&mut self) {
        self.stop().await;
        let pool = db::open(&self.dir.path().join("cocat.db")).await.unwrap();
        self.pool = pool;
        self.serve(self.addr);
    }

    /// Serves on `addr` from a thread of its own, updating `self.addr` with the bound port.
    fn serve(&mut self, addr: SocketAddr) {
        let (stop, stopped) = oneshot::channel();
        let state = AppState {
            pool: self.pool.clone(),
            secret: "test-secret".to_owned(),
            rooms: RoomManager::new(self.rooms_dir.clone(), None),
            before_attach: None,
            login_limit: LoginLimit::default(),
            mailer: None,
            links: Links::default(),
            oidc: Vec::new(),
            web_dir: None,
        };
        let (ready, listening) = std::sync::mpsc::channel();
        let logins = Arc::clone(&self.logins);
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = TcpListener::bind(addr).await.unwrap();
                ready.send(listener.local_addr().unwrap()).unwrap();
                let counter = move |request: axum::extract::Request,
                                    next: axum::middleware::Next| {
                    let logins = Arc::clone(&logins);
                    async move {
                        if request.uri().path().ends_with("/auth/jwt/login") {
                            logins.fetch_add(1, Ordering::SeqCst);
                        }
                        next.run(request).await
                    }
                };
                tokio::select! {
                    () = async { let _ = stopped.await; } => {}
                    served = axum::serve(
                        listener,
                        ws::router(state).layer(axum::middleware::from_fn(counter)),
                    ) => {
                        served.expect("the server keeps serving");
                    }
                }
            });
        });
        self.addr = listening.recv().expect("the server bound its port");
        self.stop = Some(stop);
        self.thread = Some(thread);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

/// Answers every websocket handshake with 403 and counts the attempts.
pub struct Refuser {
    pub addr: SocketAddr,
    pub hits: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Refuser {
    pub async fn start() -> Refuser {
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let app = Router::new().route(
            "/room/{room_id}",
            get(move || {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    StatusCode::FORBIDDEN
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { addr, hits, task }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for Refuser {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn cache() -> TempDir {
    TempDir::new().unwrap()
}

pub async fn user(server: &Server, email: &str) -> Uuid {
    db::create_user(&server.pool, email, PASSWORD, false, true)
        .await
        .unwrap()
}

pub async fn room(server: &Server, room_id: &str, owner: Uuid) {
    db::create_room(&server.pool, owner, room_id, "a room")
        .await
        .unwrap();
}

pub async fn member(server: &Server, room_id: &str, user: Uuid, role: Role) {
    db::add_member(&server.pool, room_id, user, role)
        .await
        .unwrap();
}

pub async fn token(server: &Server, user: Uuid) -> String {
    auth::start_session(&server.pool, user).await.unwrap()
}

/// A user who owns one room and holds a session token for it.
pub async fn seeded(server: &Server, room_id: &str, email: &str) -> String {
    let owner = user(server, email).await;
    room(server, room_id, owner).await;
    token(server, owner).await
}

pub fn new_catalogue(uuid: &str, name: &str) -> NewCatalogue {
    NewCatalogue {
        uuid: uuid.to_owned(),
        name: name.to_owned(),
        author: Some("a@x.org".to_owned()),
        ..NewCatalogue::default()
    }
}

pub fn new_event(uuid: &str, start: &str, stop: &str) -> EventRecord {
    let mut event = EventRecord::new(
        uuid,
        Timestamp::parse(start).unwrap(),
        Timestamp::parse(stop).unwrap(),
    );
    event.author = Some("a@x.org".to_owned());
    event
}

pub fn create_catalogue(room: &Room, uuid: &str, name: &str) {
    room.write(|txn| model::create_catalogue(txn, &new_catalogue(uuid, name)).unwrap())
        .unwrap();
}

pub fn create_event(room: &Room, uuid: &str, start: &str, stop: &str) {
    room.write(|txn| model::create_event(txn, &new_event(uuid, start, stop)).unwrap())
        .unwrap();
}

pub fn names(room: &Room) -> Vec<String> {
    let doc = room.doc();
    let txn = doc.transact();
    model::catalogues(&txn)
        .into_iter()
        .map(|catalogue| catalogue.name)
        .collect()
}

pub fn has_catalogue(room: &Room, uuid: &str) -> bool {
    let doc = room.doc();
    let txn = doc.transact();
    model::catalogue(&txn, uuid).is_some()
}

pub fn has_event(room: &Room, uuid: &str) -> bool {
    let doc = room.doc();
    let txn = doc.transact();
    model::catalogues(&txn).iter().any(|catalogue| {
        model::catalogue_events(&txn, &catalogue.uuid)
            .and_then(Result::ok)
            .is_some_and(|events| events.iter().any(|event| event.uuid == uuid))
    })
}

pub fn batches(room: &Room) -> broadcast::Receiver<Vec<Change>> {
    room.changes()
}

/// The bytes the server answers a client whose document holds `state`: its SyncStep2 reply,
/// which is the full room for an empty state and the difference for any other.
pub async fn sync_step2_bytes(
    addr: SocketAddr,
    token: &str,
    room_id: &str,
    state: StateVector,
) -> usize {
    let url = format!("ws://{addr}/room/{room_id}");
    let mut request = url.as_str().into_client_request().unwrap();
    request.headers_mut().insert(
        "sec-websocket-protocol",
        cocat_core::SUBPROTOCOL.parse().unwrap(),
    );
    request.headers_mut().insert(
        axum::http::header::COOKIE,
        HeaderValue::from_str(&format!("fastapiusersauth={token}")).unwrap(),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let hello = Message::Sync(SyncMessage::SyncStep1(state)).encode_v1();
    socket.send(Frame::Binary(hello.into())).await.unwrap();
    loop {
        let frame = timeout(WAIT, socket.next())
            .await
            .expect("the server did not answer")
            .expect("the socket closed before the reply")
            .expect("the socket broke before the reply");
        if let Frame::Binary(bytes) = frame
            && let Ok(Message::Sync(SyncMessage::SyncStep2(update))) = Message::decode_v1(&bytes)
        {
            return update.len();
        }
    }
}

pub async fn next_batch(changes: &mut broadcast::Receiver<Vec<Change>>) -> Vec<Change> {
    timeout(WAIT, changes.recv())
        .await
        .expect("no change batch arrived")
        .expect("the change channel closed")
}
