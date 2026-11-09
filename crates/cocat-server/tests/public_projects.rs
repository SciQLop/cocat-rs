//! Public catalogues: projects with a `public.` id that every signed-in account reads and
//! curators edit. Nobody owns, joins or shares them.

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use cocat_core::model::{self, EventRecord};
use cocat_core::role::Role;
use cocat_core::store::FileRoomStore;
use cocat_core::time::Timestamp;
use cocat_server::history::stamps::{StampFile, stamp_for};
use cocat_server::room::{Author, RoomManager};
use cocat_server::{AppState, auth, db, groups, links::Links, ratelimit::LoginLimit, ws};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tower::ServiceExt;
use uuid::Uuid;
use yrs::{Doc, ReadTxn, Transact};

/// A server with a reader (any signed-in account), a marked curator and an administrator.
struct World {
    state: AppState,
    update_dir: PathBuf,
    reader: Uuid,
    curator: Uuid,
    admin: Uuid,
    _dir: TempDir,
}

impl World {
    async fn start() -> Self {
        let dir = TempDir::new().unwrap();
        let update_dir = dir.path().join("rooms");
        fs::create_dir_all(&update_dir).unwrap();
        let pool = db::open(&dir.path().join("test.db")).await.unwrap();
        let state = AppState {
            pool,
            secret: "test-secret".to_owned(),
            rooms: RoomManager::new(update_dir.clone(), None),
            before_attach: None,
            login_limit: LoginLimit::default(),
            mailer: None,
            links: Links::default(),
            oidc: Vec::new(),
            web_dir: None,
        };
        let reader = db::create_user(&state.pool, "reader@example.org", "pwd", false, true)
            .await
            .unwrap();
        let curator = db::create_user(&state.pool, "curator@example.org", "pwd", false, true)
            .await
            .unwrap();
        db::set_curator(&state.pool, curator, true).await.unwrap();
        let admin = db::create_user(&state.pool, "admin@example.org", "pwd", true, true)
            .await
            .unwrap();
        Self {
            state,
            update_dir,
            reader,
            curator,
            admin,
            _dir: dir,
        }
    }

    /// A public catalogue made by the curator through the API; its id.
    async fn public(&self, body: Value) -> String {
        let (status, created) = self
            .call(Some(self.curator), Method::POST, "/api/public", Some(body))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        created["id"].as_str().unwrap().to_owned()
    }

    async fn call(
        &self,
        caller: Option<Uuid>,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(caller) = caller {
            let token = auth::start_session(&self.state.pool, caller).await.unwrap();
            request = request.header(header::COOKIE, format!("fastapiusersauth={token}"));
        }
        let body = match body {
            Some(body) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        let response = ws::router(self.state.clone())
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// One event and a catalogue holding it, as the curator's first version.
    fn seed(&self, room: &str) {
        let doc = Doc::new();
        let record = {
            let before = doc.transact().state_vector();
            let mut txn = doc.transact_mut();
            let at = Timestamp::parse("2020-01-01T00:00:00Z").unwrap();
            model::create_event(&mut txn, &EventRecord::new("e1", at, at)).unwrap();
            bytes::Bytes::from(txn.encode_diff_v1(&before))
        };
        let (mut store, _) = FileRoomStore::open(&self.update_dir, room).unwrap();
        store.append(record.clone()).unwrap();
        store.flush().unwrap();
        let mut stamps = StampFile::open(&self.update_dir, room).unwrap();
        stamps.append(stamp_for(0, Some(self.curator), 0, &record));
        stamps.write().unwrap();
    }
}

#[tokio::test]
async fn every_signed_in_account_reads_and_curators_edit() {
    let world = World::start().await;
    let id = world.public(json!({"name": "Shocks"})).await;
    let stranger = db::create_user(&world.state.pool, "x@example.org", "pwd", false, true)
        .await
        .unwrap();

    let role = |user| db::role_in(&world.state.pool, &id, user);
    assert_eq!(role(world.reader).await.unwrap(), Some(Role::Viewer));
    assert_eq!(role(stranger).await.unwrap(), Some(Role::Viewer));
    assert_eq!(role(world.curator).await.unwrap(), Some(Role::Editor));
    assert_eq!(role(world.admin).await.unwrap(), Some(Role::Editor));
    assert_eq!(role(Uuid::new_v4()).await.unwrap(), None);

    db::set_curator(&world.state.pool, world.curator, false)
        .await
        .unwrap();
    assert_eq!(role(world.curator).await.unwrap(), Some(Role::Viewer));
}

#[tokio::test]
async fn only_curators_create_and_the_id_is_reserved_for_them() {
    let world = World::start().await;
    let (status, body) = world
        .call(
            Some(world.reader),
            Method::POST,
            "/api/public",
            Some(json!({"name": "Mine"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["detail"], "Only curators can create public catalogues");
    let (status, _) = world
        .call(
            None,
            Method::POST,
            "/api/public",
            Some(json!({"name": "x"})),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, created) = world
        .call(
            Some(world.admin),
            Method::POST,
            "/api/public",
            Some(json!({
                "name": " Solar wind lists ",
                "folder": "AMDA/Solar wind",
                "source": "AMDA",
                "description": "Shocks seen by ACE",
                "key": "amda.sharedcatalog_22"
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["id"], "public.amda.sharedcatalog_22");
    assert_eq!(created["name"], "Solar wind lists");
    assert_eq!(created["folder"], "AMDA/Solar wind");
    assert_eq!(created["role"], "editor");

    let generated = world.public(json!({"name": "Plain"})).await;
    assert!(generated.starts_with("public."), "{generated}");
    assert!(cocat_core::room_id::is_public_id(&generated));

    for (body, wanted) in [
        (
            json!({"name": "again", "key": "amda.sharedcatalog_22"}),
            StatusCode::CONFLICT,
        ),
        (
            json!({"name": "bad", "key": "../up"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"name": "bad", "key": ".lead"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"name": "bad", "key": "a..b"}),
            StatusCode::BAD_REQUEST,
        ),
        (json!({"name": "   "}), StatusCode::BAD_REQUEST),
        (
            json!({"name": "ok", "folder": "x".repeat(201)}),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (status, answer) = world
            .call(
                Some(world.curator),
                Method::POST,
                "/api/public",
                Some(body.clone()),
            )
            .await;
        assert_eq!(status, wanted, "{body}: {answer}");
    }
}

#[tokio::test]
async fn the_index_lists_them_for_everyone_signed_in_and_the_project_list_does_not() {
    let world = World::start().await;
    let id = world
        .public(json!({"name": "Shocks", "folder": "AMDA", "source": "AMDA", "description": "d"}))
        .await;
    world.seed(&id);

    let (status, index) = world
        .call(Some(world.reader), Method::GET, "/api/public", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let entry = &index[0];
    assert_eq!(entry["id"], id.as_str());
    assert_eq!(entry["name"], "Shocks");
    assert_eq!(entry["folder"], "AMDA");
    assert_eq!(entry["source"], "AMDA");
    assert_eq!(entry["description"], "d");
    assert_eq!(entry["role"], "viewer");
    assert!(entry["changed"].as_str().is_some(), "{entry}");

    let (_, index) = world
        .call(Some(world.curator), Method::GET, "/api/public", None)
        .await;
    assert_eq!(index[0]["role"], "editor");
    let (status, _) = world.call(None, Method::GET, "/api/public", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    for caller in [world.reader, world.curator] {
        let (_, rooms) = world
            .call(Some(caller), Method::GET, "/api/rooms", None)
            .await;
        assert!(
            rooms
                .as_array()
                .unwrap()
                .iter()
                .all(|room| room["id"] != id.as_str()),
            "{rooms}"
        );
    }
}

/// What a reader, a curator and an administrator get on each route of a public project.
#[tokio::test]
async fn the_routes_answer_as_the_roles_say() {
    let world = World::start().await;
    let id = world.public(json!({"name": "Shocks"})).await;
    world.seed(&id);
    let group = groups::create(&world.state, world.curator, "team")
        .await
        .unwrap();
    let member = world.reader.to_string();
    let conflict = 409;
    // (method, path, body, [reader, curator, admin])
    let rows: Vec<(Method, String, Option<Value>, [u16; 3])> = vec![
        (Method::GET, "catalogues".into(), None, [200, 200, 200]),
        (Method::GET, "history".into(), None, [200, 200, 200]),
        (Method::GET, "history/0".into(), None, [200, 200, 200]),
        (Method::GET, "history/0/state".into(), None, [200, 200, 200]),
        (
            Method::POST,
            "history/commit".into(),
            Some(json!({"message": "m"})),
            [403, 200, 409],
        ),
        (
            Method::PUT,
            "history/0/name".into(),
            Some(json!({"name": "v1"})),
            [403, 204, 204],
        ),
        (Method::GET, "members".into(), None, [conflict; 3]),
        (
            Method::PATCH,
            format!("members/{member}"),
            Some(json!({"role": "editor"})),
            [conflict; 3],
        ),
        (
            Method::DELETE,
            format!("members/{member}"),
            None,
            [conflict; 3],
        ),
        (
            Method::POST,
            "owner".into(),
            Some(json!({"user_id": member})),
            [conflict; 3],
        ),
        (
            Method::PUT,
            "editors-can-share".into(),
            Some(json!({"allowed": false})),
            [conflict; 3],
        ),
        (Method::GET, "invitations".into(), None, [conflict; 3]),
        (
            Method::POST,
            "invitations".into(),
            Some(json!({"role": "viewer"})),
            [conflict; 3],
        ),
        (Method::DELETE, "invitations/1".into(), None, [conflict; 3]),
        (Method::GET, "suggestions".into(), None, [conflict; 3]),
        (Method::GET, "groups".into(), None, [conflict; 3]),
        (
            Method::PUT,
            format!("groups/{}", group.id),
            Some(json!({"role": "viewer"})),
            [conflict; 3],
        ),
        (
            Method::DELETE,
            format!("groups/{}", group.id),
            None,
            [conflict; 3],
        ),
        (
            Method::PATCH,
            String::new(),
            Some(json!({"name": "Bow shocks"})),
            [403, 200, 200],
        ),
    ];
    let mut wrong = Vec::new();
    for (method, tail, body, wanted) in &rows {
        let path = match tail.as_str() {
            "" => format!("/api/rooms/{id}"),
            tail => format!("/api/rooms/{id}/{tail}"),
        };
        let (status, _) = world.call(None, method.clone(), &path, body.clone()).await;
        if status != StatusCode::UNAUTHORIZED {
            wrong.push(format!("{method} {path} anonymous: {status}"));
        }
        for (caller, wanted) in [world.reader, world.curator, world.admin]
            .into_iter()
            .zip(wanted)
        {
            let (status, answer) = world
                .call(Some(caller), method.clone(), &path, body.clone())
                .await;
            if status.as_u16() != *wanted {
                wrong.push(format!(
                    "{method} {path} as {caller}: {status} {answer}, wanted {wanted}"
                ));
            }
        }
    }
    let (status, answer) = world
        .call(
            Some(world.reader),
            Method::GET,
            &format!("/room/{id}/users"),
            None,
        )
        .await;
    if status != StatusCode::CONFLICT {
        wrong.push(format!("legacy users: {status} {answer}"));
    }
    assert_eq!(wrong, Vec::<String>::new());

    let (_, refused) = world
        .call(
            Some(world.admin),
            Method::GET,
            &format!("/api/rooms/{id}/members"),
            None,
        )
        .await;
    assert_eq!(
        refused["detail"],
        "A public catalogue has no members: it cannot be shared, left or handed over"
    );
}

#[tokio::test]
async fn a_reader_copies_a_public_version_into_a_project_of_their_own() {
    let world = World::start().await;
    let id = world.public(json!({"name": "Shocks"})).await;
    world.seed(&id);

    let (status, copied) = world
        .call(
            Some(world.reader),
            Method::POST,
            &format!("/api/rooms/{id}/history/0/copy"),
            Some(json!({"name": "My shocks"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{copied}");
    let copy = copied["room"].as_str().unwrap();
    assert_eq!(
        db::role_in(&world.state.pool, copy, world.reader)
            .await
            .unwrap(),
        Some(Role::Owner)
    );
}

#[tokio::test]
async fn only_curators_delete_one() {
    let world = World::start().await;
    let id = world.public(json!({"name": "Shocks"})).await;
    let path = format!("/api/rooms/{id}");
    let (status, _) = world
        .call(Some(world.reader), Method::DELETE, &path, None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = world
        .call(Some(world.curator), Method::DELETE, &path, None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!db::room_exists(&world.state.pool, &id).await.unwrap());
}

#[tokio::test]
async fn a_public_project_can_never_get_members_invitations_or_groups() {
    let world = World::start().await;
    let id = world.public(json!({"name": "Shocks"})).await;
    let pool = &world.state.pool;
    assert!(
        db::add_member(pool, &id, world.reader, Role::Viewer)
            .await
            .is_err()
    );
    let group = groups::create(&world.state, world.curator, "team")
        .await
        .unwrap();
    let granted =
        sqlx::query("INSERT INTO room_group (room_id, group_id, role) VALUES (?, ?, 'viewer')")
            .bind(&id)
            .bind(&group.id)
            .execute(pool)
            .await;
    assert!(granted.is_err());
    let invited = sqlx::query(
        "INSERT INTO invitation (token_hash, room_id, role, expires_at) VALUES ('h', ?, 'viewer', '2999-01-01')",
    )
    .bind(&id)
    .execute(pool)
    .await;
    assert!(invited.is_err());
}

#[tokio::test]
async fn ids_with_the_reserved_prefix_that_are_not_public_stop_the_server() {
    let world = World::start().await;
    world.public(json!({"name": "Fine"})).await;
    db::refuse_reserved_ids(&world.state.pool).await.unwrap();

    sqlx::query("INSERT INTO room (id, name) VALUES ('public.lab', 'Lab')")
        .execute(&world.state.pool)
        .await
        .unwrap();
    let error = db::refuse_reserved_ids(&world.state.pool)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("public.lab"), "{error}");
    assert!(error.contains("public catalogues"), "{error}");
}

#[tokio::test]
async fn the_server_refuses_to_start_on_a_reserved_id() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let pool = db::open(&db_path).await.unwrap();
    sqlx::query("INSERT INTO room (id, name) VALUES ('public.lab', 'Lab')")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cocat-server"))
        .args([
            "serve",
            "--host",
            "127.0.0.1",
            "--port",
            "0",
            "--update_dir",
        ])
        .arg(dir.path().join("rooms"))
        .arg("--db_path")
        .arg(&db_path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("public.lab"), "{stderr}");
}

#[tokio::test]
async fn the_socket_names_the_role_and_a_curator_unmarked_is_cut_off() {
    let world = World::start().await;
    let id = world.public(json!({"name": "Shocks"})).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = ws::router(world.state.clone());
    let _server = tokio::spawn(async move { axum::serve(listener, app).await });
    let request = |token: Option<String>| {
        let mut request = format!("ws://{address}/room/{id}")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "sec-websocket-protocol",
            cocat_core::SUBPROTOCOL.parse().unwrap(),
        );
        if let Some(token) = token {
            request.headers_mut().insert(
                header::COOKIE,
                format!("fastapiusersauth={token}").parse().unwrap(),
            );
        }
        request
    };
    let token = |user| auth::start_session(&world.state.pool, user);

    let (_, answer) = connect_async(request(Some(token(world.reader).await.unwrap())))
        .await
        .unwrap();
    assert_eq!(answer.headers()[cocat_core::ROLE_HEADER], "viewer");
    match connect_async(request(None)).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status().as_u16(), 401)
        }
        other => panic!("an anonymous socket was not refused: {other:?}"),
    }

    let (mut socket, answer) = connect_async(request(Some(token(world.curator).await.unwrap())))
        .await
        .unwrap();
    assert_eq!(answer.headers()[cocat_core::ROLE_HEADER], "editor");
    socket.next().await;
    let admin = auth::start_session(&world.state.pool, world.admin)
        .await
        .unwrap();
    let response = ws::router(world.state.clone())
        .oneshot(
            Request::post(format!("/ui/admin/users/{}/flag", world.curator))
                .header(header::COOKIE, format!("fastapiusersauth={admin}"))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("flag=curator&value=false"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(response.status().is_redirection());
    assert!(
        !db::is_curator(&world.state.pool, world.curator)
            .await
            .unwrap()
    );
    let close = loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
            .await
            .expect("the socket stayed open")
        {
            Some(Ok(WsMessage::Close(frame))) => break frame,
            Some(Ok(_)) => continue,
            other => panic!("expected a close, got {other:?}"),
        }
    };
    assert_eq!(close.map(|frame| u16::from(frame.code)), Some(4403));
}

#[tokio::test]
async fn a_curator_changes_what_the_index_says_and_nobody_else_can() {
    let world = World::start().await;
    let id = world
        .public(json!({"name": "Shocks", "folder": "AMDA", "source": "AMDA", "description": "d"}))
        .await;
    let path = format!("/api/public/{id}");

    let (status, changed) = world
        .call(
            Some(world.curator),
            Method::PATCH,
            &path,
            Some(json!({"name": " ACE shocks ", "folder": "AMDA/Solar wind", "description": ""})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(changed["id"], id.as_str());
    assert_eq!(changed["name"], "ACE shocks");
    assert_eq!(changed["folder"], "AMDA/Solar wind");
    assert_eq!(changed["source"], "AMDA", "a field left out stays");
    assert_eq!(changed["description"], Value::Null, "a blank field clears");
    let (_, index) = world
        .call(Some(world.reader), Method::GET, "/api/public", None)
        .await;
    assert_eq!(index[0]["name"], "ACE shocks");
    assert_eq!(index[0]["folder"], "AMDA/Solar wind");

    let rename = Some(json!({"name": "mine"}));
    let (status, body) = world
        .call(Some(world.reader), Method::PATCH, &path, rename.clone())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["detail"], "Only curators can change public catalogues");
    let (status, _) = world.call(None, Method::PATCH, &path, rename.clone()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = world
        .call(Some(world.admin), Method::PATCH, &path, rename.clone())
        .await;
    assert_eq!(status, StatusCode::OK);

    db::create_room(&world.state.pool, world.curator, "lab", "Lab")
        .await
        .unwrap();
    for (target, body, wanted) in [
        (
            "/api/public/public.nope",
            rename.clone(),
            StatusCode::NOT_FOUND,
        ),
        ("/api/public/lab", rename.clone(), StatusCode::NOT_FOUND),
        (
            path.as_str(),
            Some(json!({"name": "  "})),
            StatusCode::BAD_REQUEST,
        ),
        (
            path.as_str(),
            Some(json!({"folder": "x".repeat(201)})),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (status, answer) = world
            .call(Some(world.curator), Method::PATCH, target, body.clone())
            .await;
        assert_eq!(status, wanted, "{target} {body:?}: {answer}");
    }
}

#[tokio::test]
async fn the_index_counts_the_events_and_follows_their_changes() {
    let world = World::start().await;
    let id = world.public(json!({"name": "Shocks"})).await;
    let count = || async {
        let (_, index) = world
            .call(Some(world.reader), Method::GET, "/api/public", None)
            .await;
        index[0]["count"].clone()
    };
    assert_eq!(count().await, json!(0), "an empty catalogue holds none");

    world.seed(&id);
    assert_eq!(count().await, json!(1));
    assert_eq!(count().await, json!(1), "the stored count answers again");

    let doc = Doc::new();
    let update = {
        let mut txn = doc.transact_mut();
        let at = Timestamp::parse("2021-01-01T00:00:00Z").unwrap();
        model::create_event(&mut txn, &EventRecord::new("e2", at, at)).unwrap();
        model::create_event(&mut txn, &EventRecord::new("e3", at, at)).unwrap();
        model::delete_event(&mut txn, "e3");
        txn.encode_update_v1()
    };
    let (mut store, _) = FileRoomStore::open(&world.update_dir, &id).unwrap();
    store.append(bytes::Bytes::from(update)).unwrap();
    store.flush().unwrap();
    assert_eq!(count().await, json!(2), "the trash does not count");
}

/// One event as an update `author` writes into `room`.
async fn write_event(world: &World, room: &str, author: Uuid, uuid: &str) {
    let doc = Doc::new();
    let at = Timestamp::parse("2020-01-01T00:00:00Z").unwrap();
    model::create_event(&mut doc.transact_mut(), &EventRecord::new(uuid, at, at)).unwrap();
    let update = doc
        .transact()
        .encode_state_as_update_v1(&Default::default());
    world
        .state
        .rooms
        .write_as(room, u64::MAX, Author::Client(author), update)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_readers_history_names_curators_and_contributors_never_their_emails() {
    let world = World::start().await;
    let id = world.public(json!({"name": "Shocks"})).await;
    write_event(&world, &id, world.curator, "by-curator").await;
    write_event(&world, &id, world.reader, "by-contributor").await;
    let path = format!("/api/rooms/{id}/history");

    let (status, read) = world
        .call(Some(world.reader), Method::GET, &path, None)
        .await;
    let (_, curated) = world
        .call(Some(world.curator), Method::GET, &path, None)
        .await;

    assert_eq!(status, StatusCode::OK, "{read}");
    assert!(!read.to_string().contains('@'), "{read}");
    let authors = |history: &Value| -> Vec<String> {
        history["versions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|version| version["author_email"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(authors(&read), ["a contributor", "a curator"]);
    assert_eq!(
        authors(&curated),
        ["reader@example.org", "curator@example.org"]
    );
    let author_ids = |history: &Value| -> Vec<Value> {
        history["versions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|version| version["author"].clone())
            .collect()
    };
    assert_eq!(author_ids(&read), [Value::Null, Value::Null]);
    assert_eq!(
        author_ids(&curated),
        [
            json!(world.reader.to_string()),
            json!(world.curator.to_string())
        ]
    );
}

/// What `set-curator --revoke` does from its own process: it can only reach the database, so
/// the server's periodic role check is what closes the demoted curator's socket.
#[tokio::test]
async fn a_curator_unmarked_from_the_command_line_is_cut_off_at_the_next_role_check() {
    let world = World::start().await;
    let id = world.public(json!({"name": "Shocks"})).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = ws::router(world.state.clone());
    let _server = tokio::spawn(async move { axum::serve(listener, app).await });
    let token = auth::start_session(&world.state.pool, world.curator)
        .await
        .unwrap();
    let mut request = format!("ws://{address}/room/{id}")
        .into_client_request()
        .unwrap();
    let headers = request.headers_mut();
    headers.insert(
        "sec-websocket-protocol",
        cocat_core::SUBPROTOCOL.parse().unwrap(),
    );
    headers.insert(
        header::COOKIE,
        format!("fastapiusersauth={token}").parse().unwrap(),
    );
    let (mut socket, _) = connect_async(request).await.unwrap();
    socket.next().await;

    ws::check_roles(&world.state).await;
    assert_eq!(world.state.rooms.clients_of(&id).await, 1, "a curator kept");
    db::set_curator(&world.state.pool, world.curator, false)
        .await
        .unwrap();
    ws::check_roles(&world.state).await;

    let close = loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
            .await
            .expect("the socket stayed open")
        {
            Some(Ok(WsMessage::Close(frame))) => break frame,
            Some(Ok(_)) => continue,
            other => panic!("expected a close, got {other:?}"),
        }
    };
    assert_eq!(close.map(|frame| u16::from(frame.code)), Some(4403));
}
