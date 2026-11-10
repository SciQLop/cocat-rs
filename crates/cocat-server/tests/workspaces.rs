use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use bytes::Bytes;
use cocat_core::role::Role;
use cocat_core::store::room_file;
use cocat_server::room::RoomManager;
use cocat_server::{AppState, auth, db, links::Links, ratelimit::LoginLimit, ws};
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
use yrs::{Doc, Map, ReadTxn, StateVector, Transact};

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
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body)
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

    async fn project(&self, token: &str, kind: &str) -> String {
        let (status, body) = self
            .send(
                Method::POST,
                "/api/rooms",
                token,
                Some(json!({"name": "Lab", "kind": kind})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["id"].as_str().unwrap().to_owned()
    }

    async fn add_document(&self, token: &str, room: &str, kind: &str) -> (StatusCode, Value) {
        self.send(
            Method::POST,
            &format!("/api/rooms/{room}/documents"),
            token,
            Some(json!({"kind": kind})),
        )
        .await
    }
}

#[tokio::test]
async fn a_workspace_starts_with_its_manifest() {
    let fixture = Fixture::start().await;
    let (_, token) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&token, "workspace").await;

    let (status, body) = fixture
        .send(
            Method::GET,
            &format!("/api/rooms/{room}/documents"),
            &token,
            None,
        )
        .await;

    assert_eq!(status, StatusCode::OK);
    let documents = body.as_array().unwrap();
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0]["kind"], "manifest");
    assert!(documents[0]["id"].is_string());
    assert!(documents[0]["created_by"].is_string());
    assert!(documents[0]["created_at"].is_string());
}

#[tokio::test]
async fn an_editor_adds_a_notebook() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let (editor, editor_token) = fixture.user_token("editor@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    db::add_member(&fixture.state.pool, &room, editor, Role::Editor)
        .await
        .unwrap();

    let (status, body) = fixture.add_document(&editor_token, &room, "notebook").await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["kind"], "notebook");
    let id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        db::document_kind(&fixture.state.pool, &room, id)
            .await
            .unwrap(),
        Some(db::DocumentKind::Notebook)
    );
    let (_, listed) = fixture
        .send(
            Method::GET,
            &format!("/api/rooms/{room}/documents"),
            &owner,
            None,
        )
        .await;
    assert_eq!(listed.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn a_viewer_cannot_add_a_document() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let (viewer, viewer_token) = fixture.user_token("viewer@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    db::add_member(&fixture.state.pool, &room, viewer, Role::Viewer)
        .await
        .unwrap();

    let (status, _) = fixture.add_document(&viewer_token, &room, "text").await;
    let (read, _) = fixture
        .send(
            Method::GET,
            &format!("/api/rooms/{room}/documents"),
            &viewer_token,
            None,
        )
        .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(read, StatusCode::OK);
}

#[tokio::test]
async fn files_need_the_file_list_first() {
    let fixture = Fixture::start().await;
    let (_, token) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&token, "catalogues").await;

    let (status, body) = fixture.add_document(&token, &room, "notebook").await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["detail"], "This project has no files yet");
}

#[tokio::test]
async fn a_plain_project_gains_its_file_list_once() {
    let fixture = Fixture::start().await;
    let (_, token) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&token, "catalogues").await;
    assert!(!db::has_files(&fixture.state.pool, &room).await.unwrap());

    let (first, made) = fixture.add_document(&token, &room, "manifest").await;
    let (second, refused) = fixture.add_document(&token, &room, "manifest").await;
    let (notebook, _) = fixture.add_document(&token, &room, "notebook").await;

    assert_eq!(first, StatusCode::CREATED, "{made}");
    assert_eq!(made["kind"], "manifest");
    assert_eq!(second, StatusCode::CONFLICT);
    assert_eq!(refused["detail"], "This project already has its file list");
    assert_eq!(notebook, StatusCode::CREATED);
    assert!(db::has_files(&fixture.state.pool, &room).await.unwrap());
}

#[tokio::test]
async fn a_workspace_is_made_with_its_file_list() {
    let fixture = Fixture::start().await;
    let (_, token) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&token, "workspace").await;

    let (status, refused) = fixture.add_document(&token, &room, "manifest").await;

    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
}

#[tokio::test]
async fn the_project_list_says_which_projects_have_files() {
    let fixture = Fixture::start().await;
    let (_, token) = fixture.user_token("owner@example.org").await;
    let workspace = fixture.project(&token, "workspace").await;
    let (status, created) = fixture
        .send(
            Method::POST,
            "/api/rooms",
            &token,
            Some(json!({"name": "Plain"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["files"], false);

    let (_, listed) = fixture.send(Method::GET, "/api/rooms", &token, None).await;

    let files_of = |id: &str| {
        listed
            .as_array()
            .unwrap()
            .iter()
            .find(|room| room["id"] == id)
            .map(|room| room["files"].clone())
    };
    assert_eq!(files_of(&workspace), Some(json!(true)));
    assert_eq!(
        files_of(created["id"].as_str().unwrap()),
        Some(json!(false))
    );
}

async fn handshake(addr: SocketAddr, path: &str, token: &str) -> Result<Socket, u16> {
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
        Ok((socket, _)) => Ok(socket),
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            Err(response.status().as_u16())
        }
        Err(error) => panic!("the handshake failed: {error}"),
    }
}

async fn connect(addr: SocketAddr, path: &str, token: &str) -> Socket {
    handshake(addr, path, token)
        .await
        .unwrap_or_else(|status| panic!("the handshake was refused with {status}"))
}

async fn next_message(socket: &mut Socket) -> Message {
    let frame = timeout(WAIT, socket.next())
        .await
        .expect("the socket went quiet")
        .expect("the socket closed")
        .expect("the socket failed");
    match frame {
        WsMessage::Binary(bytes) => Message::decode_v1(&bytes).unwrap(),
        other => panic!("expected a binary frame, got {other:?}"),
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

async fn notebook(fixture: &Fixture, token: &str, room: &str) -> Uuid {
    let (status, body) = fixture.add_document(token, room, "notebook").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().parse().unwrap()
}

fn files_in(dir: &std::path::Path) -> usize {
    fs::read_dir(dir).unwrap().count()
}

#[tokio::test]
async fn two_members_co_edit_a_notebook() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let (editor, editor_token) = fixture.user_token("editor@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    db::add_member(&fixture.state.pool, &room, editor, Role::Editor)
        .await
        .unwrap();
    let doc = notebook(&fixture, &owner, &room).await;
    let addr = fixture.serve().await;
    let path = format!("{room}/{doc}");
    let mut first = connect(addr, &path, &owner).await;
    let mut second = connect(addr, &path, &editor_token).await;
    next_message(&mut first).await;
    next_message(&mut second).await;

    let payload = update_with("key", "value");
    first.send(update_frame(payload.clone())).await.unwrap();

    match next_message(&mut second).await {
        Message::Sync(SyncMessage::Update(update)) => assert_eq!(update, payload),
        other => panic!("expected an update, got {other:?}"),
    }
    let key = cocat_server::documents::key(&room, doc);
    fixture.state.rooms.flush(&key).await.unwrap();
    assert!(room_file(&fixture.rooms_dir, &key).exists());
}

#[tokio::test]
async fn a_viewer_reads_a_notebook_but_never_writes_it() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let (viewer, viewer_token) = fixture.user_token("viewer@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    db::add_member(&fixture.state.pool, &room, viewer, Role::Viewer)
        .await
        .unwrap();
    let doc = notebook(&fixture, &owner, &room).await;
    let addr = fixture.serve().await;
    let path = format!("{room}/{doc}");
    let mut writer = connect(addr, &path, &owner).await;
    let mut reader = connect(addr, &path, &viewer_token).await;
    next_message(&mut writer).await;
    next_message(&mut reader).await;

    reader
        .send(update_frame(update_with("viewer", "nope")))
        .await
        .unwrap();
    let payload = update_with("owner", "yes");
    writer.send(update_frame(payload.clone())).await.unwrap();

    match next_message(&mut reader).await {
        Message::Sync(SyncMessage::Update(update)) => assert_eq!(update, payload),
        other => panic!("the viewer should only hear the owner, got {other:?}"),
    }
}

#[tokio::test]
async fn a_stranger_is_refused_a_notebook() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let (_, stranger) = fixture.user_token("stranger@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    let doc = notebook(&fixture, &owner, &room).await;
    let addr = fixture.serve().await;

    let refused = handshake(addr, &format!("{room}/{doc}"), &stranger).await;

    assert_eq!(refused.err(), Some(403));
}

#[tokio::test]
async fn a_document_id_that_is_not_a_uuid_is_unknown() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    let addr = fixture.serve().await;
    let before = files_in(&fixture.rooms_dir);

    for bad in ["..", "a%2Fb", "not-a-uuid", &"x".repeat(300)] {
        let refused = handshake(addr, &format!("{room}/{bad}"), &owner).await;
        assert_eq!(refused.err(), Some(404), "{bad}");
    }

    assert_eq!(files_in(&fixture.rooms_dir), before);
}

#[tokio::test]
async fn an_unknown_document_is_refused() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    let addr = fixture.serve().await;

    let refused = handshake(addr, &format!("{room}/{}", Uuid::new_v4()), &owner).await;

    assert_eq!(refused.err(), Some(404));
}

#[tokio::test]
async fn a_catalogue_project_has_no_document_sockets() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let addr = fixture.serve().await;

    let refused = handshake(addr, &format!("{room}/{}", Uuid::new_v4()), &owner).await;

    assert_eq!(refused.err(), Some(404));
}

#[tokio::test]
async fn a_project_with_files_still_has_its_catalogues() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    let addr = fixture.serve().await;

    let mut socket = connect(addr, &room, &owner).await;

    next_message(&mut socket).await;
}

#[tokio::test]
async fn the_role_check_keeps_a_members_notebook_socket_open() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    let doc = notebook(&fixture, &owner, &room).await;
    let addr = fixture.serve().await;
    let mut socket = connect(addr, &format!("{room}/{doc}"), &owner).await;
    next_message(&mut socket).await;

    ws::check_roles(&fixture.state).await;

    socket
        .send(update_frame(update_with("key", "value")))
        .await
        .unwrap();
    let key = cocat_server::documents::key(&room, doc);
    fixture.state.rooms.flush(&key).await.unwrap();
    assert_eq!(fixture.state.rooms.clients_of(&key).await, 1);
}

#[tokio::test]
async fn a_slash_project_row_does_not_reach_another_projects_document_room() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let (intruder, intruder_token) = fixture.user_token("intruder@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    let doc = notebook(&fixture, &owner, &room).await;
    let fake = cocat_server::documents::key(&room, doc);
    sqlx::query("INSERT INTO room (id, name, created_by) VALUES (?, ?, ?)")
        .bind(&fake)
        .bind(&fake)
        .bind(intruder.to_string())
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO membership (room_id, user_id, role) VALUES (?, ?, 'owner')")
        .bind(&fake)
        .bind(intruder.to_string())
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    let addr = fixture.serve().await;

    let refused = handshake(addr, &room_path_escaped(&fake), &intruder_token).await;

    assert_eq!(refused.err(), Some(404));
}

fn room_path_escaped(id: &str) -> String {
    id.replace('/', "%2F")
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

#[tokio::test]
async fn removing_a_member_closes_their_notebook_sockets() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let (editor, editor_token) = fixture.user_token("editor@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    db::add_member(&fixture.state.pool, &room, editor, Role::Editor)
        .await
        .unwrap();
    let doc = notebook(&fixture, &owner, &room).await;
    let addr = fixture.serve().await;
    let mut socket = connect(addr, &format!("{room}/{doc}"), &editor_token).await;
    next_message(&mut socket).await;

    let (status, _) = fixture
        .send(
            Method::DELETE,
            &format!("/api/rooms/{room}/members/{editor}"),
            &owner,
            None,
        )
        .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(close_code(&mut socket).await, Some(4403));
}

#[tokio::test]
async fn lowering_a_group_grant_closes_notebook_sockets() {
    let fixture = Fixture::start().await;
    let (owner, owner_token) = fixture.user_token("owner@example.org").await;
    let (member, member_token) = fixture.user_token("member@example.org").await;
    let room = fixture.project(&owner_token, "workspace").await;
    let doc = notebook(&fixture, &owner_token, &room).await;
    let group = cocat_server::groups::create(&fixture.state, owner, "Team")
        .await
        .unwrap();
    sqlx::query("INSERT INTO group_member (group_id, user_id, role) VALUES (?, ?, 'member')")
        .bind(&group.id)
        .bind(member.to_string())
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    cocat_server::groups::grant(&fixture.state, &room, &group.id, owner, Role::Editor)
        .await
        .unwrap();
    let addr = fixture.serve().await;
    let mut socket = connect(addr, &format!("{room}/{doc}"), &member_token).await;
    next_message(&mut socket).await;

    cocat_server::groups::grant(&fixture.state, &room, &group.id, owner, Role::Viewer)
        .await
        .unwrap();

    assert_eq!(close_code(&mut socket).await, Some(4403));
}

#[tokio::test]
async fn deleting_a_workspace_deletes_every_document_file() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "workspace").await;
    let addr = fixture.serve().await;
    for _ in 0..2 {
        let doc = notebook(&fixture, &owner, &room).await;
        let mut socket = connect(addr, &format!("{room}/{doc}"), &owner).await;
        next_message(&mut socket).await;
        socket
            .send(update_frame(update_with("key", "value")))
            .await
            .unwrap();
        let key = cocat_server::documents::key(&room, doc);
        fixture.state.rooms.flush(&key).await.unwrap();
    }
    assert!(files_in(&fixture.rooms_dir) > 0);

    let (status, _) = fixture
        .send(Method::DELETE, &format!("/api/rooms/{room}"), &owner, None)
        .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(files_in(&fixture.rooms_dir), 0);
}
