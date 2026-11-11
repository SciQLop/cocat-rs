use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use bytes::Bytes;
use cocat_core::role::Role;
use cocat_server::room::{Author, RoomManager};
use cocat_server::{AppState, auth, db, documents, links::Links, ratelimit::LoginLimit, ws};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tower::ServiceExt;
use uuid::Uuid;
use yrs::sync::{Message, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, Map, ReadTxn, StateVector, Transact, Update};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
const WAIT: Duration = Duration::from_secs(10);

struct Fixture {
    state: AppState,
    rooms_dir: PathBuf,
    _dir: TempDir,
}

impl Fixture {
    async fn start() -> Self {
        let dir = TempDir::new().unwrap();
        let rooms = dir.path().join("rooms");
        fs::create_dir_all(&rooms).unwrap();
        let pool = db::open(&dir.path().join("test.db")).await.unwrap();
        let state = AppState {
            pool,
            secret: "test-secret".to_owned(),
            rooms: RoomManager::new(rooms.clone(), None),
            before_attach: None,
            login_limit: LoginLimit::default(),
            mailer: None,
            links: Links::default(),
            oidc: Vec::new(),
            web_dir: None,
            providers: Default::default(),
            http: Default::default(),
        };
        Self {
            state,
            rooms_dir: rooms,
            _dir: dir,
        }
    }

    async fn user_token(&self, email: &str) -> (Uuid, String) {
        let user = db::create_user(&self.state.pool, email, "hunter2", false, true)
            .await
            .unwrap();
        let token = auth::start_session(&self.state.pool, user).await.unwrap();
        (user, token)
    }

    async fn member(&self, room: &str, email: &str, role: Role) -> (Uuid, String) {
        let (user, token) = self.user_token(email).await;
        db::add_member(&self.state.pool, room, user, role)
            .await
            .unwrap();
        (user, token)
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        token: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::COOKIE, format!("fastapiusersauth={token}"));
        let body = match body {
            Some(body) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        let router: Router = ws::router(self.state.clone());
        let response = router.oneshot(request.body(body).unwrap()).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body)
    }

    async fn get(&self, path: &str, token: &str) -> (StatusCode, Value) {
        self.send(Method::GET, path, token, None).await
    }

    async fn serve(&self) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = self.state.clone();
        tokio::spawn(async move {
            axum::serve(listener, ws::router(state)).await.unwrap();
        });
        addr
    }

    async fn project(&self, token: &str) -> String {
        let (status, body) = self
            .send(
                Method::POST,
                "/api/rooms",
                token,
                Some(json!({"name": "Lab", "kind": "catalogues"})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["id"].as_str().unwrap().to_owned()
    }

    async fn round(&self, token: &str, room: &str, visibility: &str) -> String {
        let (status, body) = self
            .send(
                Method::POST,
                &format!("/api/rooms/{room}/rounds"),
                token,
                Some(json!({"name": "R", "visibility": visibility})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["id"].as_str().unwrap().to_owned()
    }

    async fn propose(&self, token: &str, room: &str, round: &str) -> (StatusCode, Value) {
        let path = format!("/api/rooms/{room}/rounds/{round}/proposal");
        self.send(Method::POST, &path, token, None).await
    }

    async fn proposal(&self, token: &str, room: &str, round: &str) -> Uuid {
        let (status, body) = self.propose(token, room, round).await;
        assert!(status.is_success(), "{status} {body}");
        body["doc"].as_str().unwrap().parse().unwrap()
    }

    async fn move_to(&self, token: &str, room: &str, round: &str, state: &str) {
        let path = format!("/api/rooms/{room}/rounds/{round}/state");
        let (status, body) = self
            .send(Method::POST, &path, token, Some(json!({"state": state})))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    async fn write(&self, room: &str, doc: Uuid, author: Uuid, key: &str) {
        let key_of_room = documents::key(room, doc);
        let allowance = self
            .state
            .rooms
            .allowance(&self.state.pool, &key_of_room)
            .await
            .unwrap();
        self.state
            .rooms
            .write_as(
                &key_of_room,
                allowance,
                Author::Client(author),
                update_with(key, "x"),
            )
            .await
            .unwrap();
    }

    async fn listed(&self, token: &str, room: &str) -> Vec<Value> {
        let (status, body) = self
            .get(&format!("/api/rooms/{room}/documents"), token)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body.as_array().unwrap().clone()
    }
}

fn lists(documents: &[Value], doc: Uuid) -> Option<&Value> {
    documents
        .iter()
        .find(|document| document["id"] == doc.to_string())
}

/// The socket and the role the server granted it, or the refusal's status.
async fn handshake(addr: SocketAddr, path: &str, token: &str) -> Result<(Socket, String), u16> {
    let mut request = format!("ws://{addr}/room/{path}")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "cookie",
        format!("fastapiusersauth={token}").parse().unwrap(),
    );
    request.headers_mut().insert(
        header::SEC_WEBSOCKET_PROTOCOL,
        cocat_core::SUBPROTOCOL.parse().unwrap(),
    );
    match connect_async(request).await {
        Ok((socket, response)) => {
            let role = response.headers()[cocat_core::ROLE_HEADER]
                .to_str()
                .unwrap()
                .to_owned();
            Ok((socket, role))
        }
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            Err(response.status().as_u16())
        }
        Err(error) => panic!("the handshake failed: {error}"),
    }
}

async fn connect(addr: SocketAddr, path: &str, token: &str) -> (Socket, String) {
    let (mut socket, role) = handshake(addr, path, token)
        .await
        .unwrap_or_else(|status| panic!("the handshake was refused with {status}"));
    next_message(&mut socket).await;
    (socket, role)
}

async fn next_message(socket: &mut Socket) -> Message {
    loop {
        let frame = timeout(WAIT, socket.next())
            .await
            .expect("the socket went quiet")
            .expect("the socket closed")
            .expect("the socket failed");
        match frame {
            WsMessage::Binary(bytes) => return Message::decode_v1(&bytes).unwrap(),
            WsMessage::Close(frame) => panic!("the socket closed: {frame:?}"),
            _ => {}
        }
    }
}

async fn close_code(socket: &mut Socket) -> Option<u16> {
    loop {
        match timeout(WAIT, socket.next()).await.expect("never closed") {
            Some(Ok(WsMessage::Close(frame))) => return frame.map(|frame| u16::from(frame.code)),
            Some(Ok(_)) => {}
            other => panic!("expected a close frame, got {other:?}"),
        }
    }
}

fn update_with(key: &str, value: &str) -> Vec<u8> {
    let doc = Doc::new();
    doc.get_or_insert_map("content")
        .insert(&mut doc.transact_mut(), key, value);
    doc.transact()
        .encode_state_as_update_v1(&StateVector::default())
}

fn update_frame(update: Vec<u8>) -> WsMessage {
    WsMessage::Binary(Bytes::from(
        Message::Sync(SyncMessage::Update(update)).encode_v1(),
    ))
}

async fn expect_update(socket: &mut Socket, payload: &[u8]) {
    loop {
        if let Message::Sync(SyncMessage::Update(update)) = next_message(socket).await {
            assert_eq!(update, payload);
            return;
        }
    }
}

/// The room's content keys as the server holds them, read after every frame sent before.
async fn content_keys(socket: &mut Socket) -> Vec<String> {
    let ask = Message::Sync(SyncMessage::SyncStep1(StateVector::default())).encode_v1();
    socket
        .send(WsMessage::Binary(Bytes::from(ask)))
        .await
        .unwrap();
    loop {
        if let Message::Sync(SyncMessage::SyncStep2(update)) = next_message(socket).await {
            let doc = Doc::new();
            let content = doc.get_or_insert_map("content");
            doc.transact_mut()
                .apply_update(Update::decode_v1(&update).unwrap())
                .unwrap();
            let txn = doc.transact();
            let mut keys: Vec<String> = content.keys(&txn).map(str::to_owned).collect();
            keys.sort();
            return keys;
        }
    }
}

#[tokio::test]
async fn a_proposer_opens_and_writes_their_proposal_socket() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner).await;
    let (_, editor) = fixture
        .member(&room, "editor@example.org", Role::Editor)
        .await;
    let round = fixture.round(&owner, &room, "open").await;
    let doc = fixture.proposal(&editor, &room, &round).await;
    assert_eq!(fixture.proposal(&editor, &room, &round).await, doc);
    let addr = fixture.serve().await;
    let path = format!("{room}/{doc}");
    let (mut author, role) = connect(addr, &path, &editor).await;
    let (mut reader, _) = connect(addr, &path, &owner).await;
    assert_eq!(role, "editor");

    let payload = update_with("interval", "1");
    author.send(update_frame(payload.clone())).await.unwrap();

    expect_update(&mut reader, &payload).await;
}

#[tokio::test]
async fn only_a_proposer_gets_a_proposal_and_only_while_proposing() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner).await;
    let (_, viewer) = fixture
        .member(&room, "viewer@example.org", Role::Viewer)
        .await;
    let round = fixture.round(&owner, &room, "open").await;

    assert_eq!(
        fixture.propose(&viewer, &room, &round).await.0,
        StatusCode::FORBIDDEN
    );
    fixture.move_to(&owner, &room, &round, "voting").await;
    assert_eq!(
        fixture.propose(&owner, &room, &round).await.0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn a_viewer_proposer_keeps_a_writable_socket_across_rechecks() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner).await;
    let (_, viewer) = fixture
        .member(&room, "viewer@example.org", Role::Viewer)
        .await;
    let round = fixture.round(&owner, &room, "open").await;
    let members = json!({
        "proposers": ["viewer@example.org"],
        "voters": [],
        "deciders": ["owner@example.org"],
    });
    let path = format!("/api/rooms/{room}/rounds/{round}/members");
    let (status, body) = fixture
        .send(Method::PUT, &path, &owner, Some(members))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let doc = fixture.proposal(&viewer, &room, &round).await;
    let addr = fixture.serve().await;
    let path = format!("{room}/{doc}");
    let (mut author, role) = connect(addr, &path, &viewer).await;
    assert_eq!(role, "editor");

    ws::check_room_roles(&fixture.state, &room).await;
    ws::check_room_roles(&fixture.state, &room).await;
    author
        .send(update_frame(update_with("interval", "1")))
        .await
        .unwrap();

    assert_eq!(content_keys(&mut author).await, ["interval"]);
}

#[tokio::test]
async fn a_blind_proposal_is_invisible_to_another_proposer_on_every_route() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner).await;
    let (alice, alice_token) = fixture
        .member(&room, "alice@example.org", Role::Editor)
        .await;
    let (_, bob) = fixture.member(&room, "bob@example.org", Role::Editor).await;
    let round = fixture.round(&owner, &room, "blind").await;
    let doc = fixture.proposal(&alice_token, &room, &round).await;
    fixture.write(&room, doc, alice, "interval").await;
    let history = format!("/api/rooms/{room}/history?document={doc}");
    let (status, own) = fixture.get(&history, &alice_token).await;
    assert_eq!(status, StatusCode::OK, "{own}");
    let version = own["versions"][0]["id"].as_u64().unwrap();
    let state = format!("/api/rooms/{room}/history/{version}/state?document={doc}");
    assert_eq!(fixture.get(&state, &alice_token).await.0, StatusCode::OK);
    let addr = fixture.serve().await;

    let socket = handshake(addr, &format!("{room}/{doc}"), &bob).await;
    assert_eq!(socket.err(), Some(403));
    assert_eq!(fixture.get(&history, &bob).await.0, StatusCode::NOT_FOUND);
    assert_eq!(fixture.get(&state, &bob).await.0, StatusCode::NOT_FOUND);
    let restore = format!("/api/rooms/{room}/history/{version}/restore");
    let (status, _) = fixture
        .send(Method::POST, &restore, &bob, Some(json!({"document": doc})))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(lists(&fixture.listed(&bob, &room).await, doc), None);
}

#[tokio::test]
async fn another_member_cannot_restore_a_proposal_they_may_read() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner).await;
    let (alice, alice_token) = fixture
        .member(&room, "alice@example.org", Role::Editor)
        .await;
    let (_, bob) = fixture.member(&room, "bob@example.org", Role::Editor).await;
    let round = fixture.round(&owner, &room, "open").await;
    let doc = fixture.proposal(&alice_token, &room, &round).await;
    fixture.write(&room, doc, alice, "interval").await;

    let restore = format!("/api/rooms/{room}/history/0/restore");
    let (status, _) = fixture
        .send(Method::POST, &restore, &bob, Some(json!({"document": doc})))
        .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_blind_round_hides_who_wrote_which_document() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner).await;
    let (_, alice) = fixture
        .member(&room, "alice@example.org", Role::Editor)
        .await;
    let (_, bob) = fixture.member(&room, "bob@example.org", Role::Editor).await;
    let round = fixture.round(&owner, &room, "blind").await;
    let alices = fixture.proposal(&alice, &room, &round).await;
    let bobs = fixture.proposal(&bob, &room, &round).await;
    let path = format!("/api/rooms/{room}/rounds/{round}");
    let doc_of = |body: &Value, author: &str| {
        body["proposals"]
            .as_array()
            .unwrap()
            .iter()
            .find(|proposal| proposal["author"] == author)
            .map(|proposal| proposal["doc"].clone())
            .unwrap()
    };

    let (_, seen_by_bob) = fixture.get(&path, &bob).await;
    let (_, seen_by_owner) = fixture.get(&path, &owner).await;

    assert_eq!(doc_of(&seen_by_bob, "alice@example.org"), Value::Null);
    assert_eq!(doc_of(&seen_by_bob, "bob@example.org"), bobs.to_string());
    assert_eq!(
        doc_of(&seen_by_owner, "alice@example.org"),
        alices.to_string()
    );
}

#[tokio::test]
async fn a_decider_and_an_owner_read_a_blind_proposal() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner).await;
    let (_, decider) = fixture
        .member(&room, "decider@example.org", Role::Editor)
        .await;
    let (alice, alice_token) = fixture
        .member(&room, "alice@example.org", Role::Editor)
        .await;
    let round = fixture.round(&decider, &room, "blind").await;
    let doc = fixture.proposal(&alice_token, &room, &round).await;
    fixture.write(&room, doc, alice, "interval").await;
    let addr = fixture.serve().await;
    let history = format!("/api/rooms/{room}/history?document={doc}");

    for reader in [&decider, &owner] {
        let (_, role) = connect(addr, &format!("{room}/{doc}"), reader).await;
        assert_eq!(role, "viewer");
        assert_eq!(fixture.get(&history, reader).await.0, StatusCode::OK);
        let listed = fixture.listed(reader, &room).await;
        assert_eq!(lists(&listed, doc).unwrap()["kind"], "proposal");
    }
}

#[tokio::test]
async fn freezing_closes_the_author_socket_and_reopens_it_read_only() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner).await;
    let (_, editor) = fixture
        .member(&room, "editor@example.org", Role::Editor)
        .await;
    let round = fixture.round(&owner, &room, "blind").await;
    let doc = fixture.proposal(&editor, &room, &round).await;
    let addr = fixture.serve().await;
    let path = format!("{room}/{doc}");
    let (mut author, _) = connect(addr, &path, &editor).await;
    author
        .send(update_frame(update_with("before", "1")))
        .await
        .unwrap();
    assert_eq!(content_keys(&mut author).await, ["before"]);

    fixture.move_to(&owner, &room, &round, "voting").await;

    assert_eq!(close_code(&mut author).await, Some(4403));
    let (mut again, role) = connect(addr, &path, &editor).await;
    assert_eq!(role, "viewer");
    again
        .send(update_frame(update_with("late", "1")))
        .await
        .unwrap();
    assert_eq!(content_keys(&mut again).await, ["before"]);
}

#[tokio::test]
async fn opening_the_round_to_all_shows_proposals_to_every_member() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner).await;
    let (_, editor) = fixture
        .member(&room, "editor@example.org", Role::Editor)
        .await;
    let (_, viewer) = fixture
        .member(&room, "viewer@example.org", Role::Viewer)
        .await;
    let round = fixture.round(&owner, &room, "blind").await;
    let doc = fixture.proposal(&editor, &room, &round).await;
    let addr = fixture.serve().await;
    let path = format!("{room}/{doc}");
    assert_eq!(handshake(addr, &path, &viewer).await.err(), Some(403));
    assert_eq!(lists(&fixture.listed(&viewer, &room).await, doc), None);

    let (status, body) = fixture
        .send(
            Method::PATCH,
            &format!("/api/rooms/{room}/rounds/{round}"),
            &owner,
            Some(json!({"visibility": "open"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (_, role) = connect(addr, &path, &viewer).await;
    assert_eq!(role, "viewer");
    let listed = fixture.listed(&viewer, &room).await;
    assert_eq!(lists(&listed, doc).unwrap()["kind"], "proposal");
}

fn big_update(bytes: usize) -> Vec<u8> {
    update_with("text", &"x".repeat(bytes))
}

#[tokio::test]
async fn deleting_a_project_removes_its_proposal_files() {
    let fixture = Fixture::start().await;
    let (owner_id, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner).await;
    let round = fixture.round(&owner, &room, "blind").await;
    let doc = fixture.proposal(&owner, &room, &round).await;
    fixture.write(&room, doc, owner_id, "interval").await;
    let key = documents::key(&room, doc);
    fixture.state.rooms.flush(&key).await.unwrap();
    assert!(cocat_core::store::room_file(&fixture.rooms_dir, &key).exists());

    let (status, _) = fixture
        .send(Method::DELETE, &format!("/api/rooms/{room}"), &owner, None)
        .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(fs::read_dir(&fixture.rooms_dir).unwrap().count(), 0);
}

#[tokio::test]
async fn proposal_bytes_count_toward_the_project_quota() {
    let fixture = Fixture::start().await;
    let (owner_id, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner).await;
    let (editor_id, editor) = fixture
        .member(&room, "editor@example.org", Role::Editor)
        .await;
    db::set_user_quota(&fixture.state.pool, owner_id, Some(1))
        .await
        .unwrap();
    let round = fixture.round(&owner, &room, "open").await;
    let rooms = &fixture.state.rooms;
    let pool = &fixture.state.pool;
    let write = |token: String, author: Uuid| {
        let (room, round) = (room.clone(), round.clone());
        let fixture = &fixture;
        async move {
            let doc = fixture.proposal(&token, &room, &round).await;
            let key = documents::key(&room, doc);
            let allowance = rooms.allowance(pool, &key).await.unwrap();
            rooms
                .write_as(&key, allowance, Author::Client(author), big_update(600_000))
                .await
        }
    };

    let first = write(owner.clone(), owner_id).await;
    let second = write(editor.clone(), editor_id).await;

    assert!(first.unwrap());
    assert!(matches!(second, Err(cocat_server::room::RoomError::Full)));
}
