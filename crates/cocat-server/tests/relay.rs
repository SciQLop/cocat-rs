//! The relay: a provider's catalogues held in the public catalogues, the provider being the
//! source of truth, and nobody editing them by hand. A fake provider stands in for the remote
//! servers.

use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use cocat_core::model::{self, EventRecord, Status};
use cocat_core::time::Timestamp;
use cocat_server::relay::{self, Fetched, Provider, RelayError, store};
use cocat_server::room::{Author, RoomManager};
use cocat_server::{AppState, auth, db, links::Links, ratelimit::LoginLimit, ws};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tower::ServiceExt;
use uuid::Uuid;
use yrs::{Doc, Transact};

const SOURCE: &str = "Fake source";
const SHOCKS: &str = "public.fake.shocks";
const STORMS: &str = "public.fake.storms";
/// The answer that makes the fake provider panic instead.
const PANIC: &str = "panic";
const FOLLOWS: &str = "This catalogue follows Fake source: it changes when Fake source does.";

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type Answer = Arc<Mutex<Result<Vec<Fetched>, String>>>;

/// Answers whatever the test put in `answer`; with `gate`, says it started and waits to be
/// let go before answering.
struct Fake {
    answer: Answer,
    gate: Option<(Arc<Notify>, Arc<Notify>)>,
}

#[async_trait]
impl Provider for Fake {
    fn id(&self) -> &'static str {
        "fake"
    }

    fn source(&self) -> &'static str {
        SOURCE
    }

    fn default_schedule(&self) -> &'static str {
        "0 3 * * *"
    }

    async fn fetch(
        &self,
        _http: &reqwest::Client,
        _settings: &Value,
        _last_ok: Option<Timestamp>,
    ) -> Result<Vec<Fetched>, String> {
        if let Some((started, release)) = &self.gate {
            started.notify_one();
            release.notified().await;
        }
        let answer = self.answer.lock().unwrap().clone();
        if answer == Err(PANIC.to_owned()) {
            panic!("the fake provider broke");
        }
        answer
    }
}

struct World {
    state: AppState,
    answer: Answer,
    reader: Uuid,
    curator: Uuid,
    author: Uuid,
    _dir: TempDir,
}

impl World {
    async fn start() -> Self {
        Self::with_gate(None).await
    }

    async fn with_gate(gate: Option<(Arc<Notify>, Arc<Notify>)>) -> Self {
        let dir = TempDir::new().unwrap();
        let update_dir = dir.path().join("rooms");
        fs::create_dir_all(&update_dir).unwrap();
        let pool = db::open(&dir.path().join("test.db")).await.unwrap();
        let answer: Answer = Arc::new(Mutex::new(Ok(two_lists())));
        let fake = Fake {
            answer: answer.clone(),
            gate,
        };
        let state = AppState {
            pool,
            secret: "test-secret".to_owned(),
            rooms: RoomManager::new(update_dir, None),
            before_attach: None,
            login_limit: LoginLimit::default(),
            mailer: None,
            links: Links::default(),
            oidc: Vec::new(),
            web_dir: None,
            providers: Arc::new(vec![Arc::new(fake)]),
            http: reqwest::Client::new(),
        };
        let account = |email: &'static str| db::create_user(&state.pool, email, "pwd", false, true);
        let reader = account("reader@example.org").await.unwrap();
        let curator = account("curator@example.org").await.unwrap();
        db::set_curator(&state.pool, curator, true).await.unwrap();
        let author = account("author@example.org").await.unwrap();
        Self {
            state,
            answer,
            reader,
            curator,
            author,
            _dir: dir,
        }
    }

    fn answer(&self, answer: Result<Vec<Fetched>, String>) {
        *self.answer.lock().unwrap() = answer;
    }

    async fn run(&self) -> Result<relay::RunReport, RelayError> {
        relay::run(&self.state, "fake").await
    }

    async fn call(
        &self,
        caller: Uuid,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let token = auth::start_session(&self.state.pool, caller).await.unwrap();
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
        let response = ws::router(self.state.clone())
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1 << 22).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn index(&self, caller: Uuid) -> Vec<Value> {
        let (status, index) = self.call(caller, Method::GET, "/api/public", None).await;
        assert_eq!(status, StatusCode::OK, "{index}");
        index.as_array().unwrap().clone()
    }

    async fn row(&self, room: &str) -> Value {
        self.index(self.reader)
            .await
            .into_iter()
            .find(|row| row["id"] == room)
            .unwrap_or_else(|| panic!("{room} is not in the index"))
    }

    async fn versions(&self, room: &str) -> Vec<Value> {
        let path = format!("/api/rooms/{room}/history");
        let (status, history) = self.call(self.reader, Method::GET, &path, None).await;
        assert_eq!(status, StatusCode::OK, "{history}");
        history["versions"].as_array().unwrap().clone()
    }

    /// The uuids of the events the catalogue lists outside the trash, sorted.
    async fn listed(&self, room: &str, catalogue: &'static str) -> Vec<String> {
        self.state
            .rooms
            .read(room, move |doc: &Doc| {
                let txn = doc.transact();
                model::listed_events(&txn, catalogue)
                    .into_iter()
                    .filter(|uuid| {
                        model::event(&txn, uuid)
                            .is_some_and(|event| event.status != Status::Deleted)
                    })
                    .collect()
            })
            .await
            .unwrap()
    }
}

fn at(day: i64) -> Timestamp {
    Timestamp::from_unix_nanos(day * 86_400_000_000_000).unwrap()
}

fn event(uuid: &str, day: i64) -> EventRecord {
    EventRecord::new(uuid, at(day), at(day + 1))
}

fn list(key: &str, catalogue: &str, events: Vec<EventRecord>) -> Fetched {
    Fetched {
        key: key.to_owned(),
        folder: "Fake/Lists".to_owned(),
        name: format!("Fake {key}"),
        description: format!("The {key} the fake provider publishes"),
        url: format!("https://fake.example.org/{key}"),
        catalogue: catalogue.to_owned(),
        columns: Vec::new(),
        events: Ok(events),
    }
}

fn shocks(events: Vec<EventRecord>) -> Fetched {
    list("shocks", "cat-shocks", events)
}

fn storms() -> Fetched {
    list("storms", "cat-storms", vec![event("s1", 10)])
}

fn two_lists() -> Vec<Fetched> {
    vec![shocks(vec![event("e1", 1), event("e2", 2)]), storms()]
}

#[tokio::test]
async fn a_first_run_makes_the_providers_catalogues_in_its_folder() {
    let world = World::start().await;

    let report = world.run().await.unwrap();

    assert_eq!(report.catalogues, 2);
    assert_eq!(report.counts.added, 3);
    assert_eq!(report.unpublished, 0);
    let row = world.row(SHOCKS).await;
    assert_eq!(row["name"], "Fake shocks");
    assert_eq!(row["folder"], "Fake/Lists");
    assert_eq!(row["source"], SOURCE);
    assert_eq!(row["description"], "The shocks the fake provider publishes");
    assert_eq!(row["count"], 2);
    assert_eq!(world.row(STORMS).await["count"], 1);
    assert_eq!(world.listed(SHOCKS, "cat-shocks").await, ["e1", "e2"]);
    let provider = store::provider(&world.state.pool, "fake")
        .await
        .unwrap()
        .unwrap();
    assert!(!provider.enabled, "a provider runs on demand, still off");
    assert!(!provider.running);
    assert!(provider.last_ok_at.is_some());
    assert_eq!(provider.last_run_at, provider.last_ok_at);
}

#[tokio::test]
async fn a_second_run_with_the_same_list_writes_nothing() {
    let world = World::start().await;
    world.run().await.unwrap();
    let before = world.versions(SHOCKS).await;

    let report = world.run().await.unwrap();

    assert!(report.counts.is_empty(), "{:?}", report.counts);
    assert_eq!(world.versions(SHOCKS).await, before);
}

#[tokio::test]
async fn a_run_writes_one_version_per_changed_catalogue_authored_by_the_source() {
    let world = World::start().await;
    world.run().await.unwrap();
    let storms_before = world.versions(STORMS).await;
    let shocks_before = world.versions(SHOCKS).await;
    world.answer(Ok(vec![
        shocks(vec![event("e1", 1), event("e3", 3)]),
        storms(),
    ]));

    let report = world.run().await.unwrap();

    assert_eq!(
        (report.counts.added, report.counts.removed),
        (1, 1),
        "{:?}",
        report.counts
    );
    assert_eq!(world.listed(SHOCKS, "cat-shocks").await, ["e1", "e3"]);
    let versions = world.versions(SHOCKS).await;
    assert_eq!(versions.len(), shocks_before.len() + 1);
    let newest = &versions[0];
    assert_eq!(newest["name"], "Refreshed from Fake source");
    assert_eq!(newest["author_email"], SOURCE);
    assert_eq!(newest["committed"], true);
    assert_eq!(world.versions(STORMS).await, storms_before);
}

#[tokio::test]
async fn a_failing_fetch_keeps_the_catalogues_and_records_the_error() {
    let world = World::start().await;
    world.run().await.unwrap();
    let ok = store::provider(&world.state.pool, "fake")
        .await
        .unwrap()
        .unwrap();
    world.answer(Err("the server answered 503".to_owned()));

    let failed = world.run().await;

    assert!(matches!(failed, Err(RelayError::Fetch(_))), "{failed:?}");
    assert_eq!(world.listed(SHOCKS, "cat-shocks").await, ["e1", "e2"]);
    let provider = store::provider(&world.state.pool, "fake")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(provider.last_ok_at, ok.last_ok_at);
    assert!(!provider.running);
    let result = provider.last_result.unwrap();
    assert!(result.contains("the server answered 503"), "{result}");
}

#[tokio::test]
async fn an_empty_fetch_keeps_the_catalogues_and_records_a_failure() {
    let world = World::start().await;
    world.run().await.unwrap();
    let ok = store::provider(&world.state.pool, "fake")
        .await
        .unwrap()
        .unwrap();
    world.answer(Ok(Vec::new()));

    let failed = world.run().await;

    assert!(matches!(failed, Err(RelayError::Empty)), "{failed:?}");
    assert_eq!(world.listed(SHOCKS, "cat-shocks").await, ["e1", "e2"]);
    assert_eq!(world.row(SHOCKS).await["relay"]["published"], true);
    let provider = store::provider(&world.state.pool, "fake")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(provider.last_ok_at, ok.last_ok_at);
    assert!(provider.last_result.unwrap().contains("error"));
}

#[tokio::test]
async fn a_list_that_cannot_be_read_is_left_as_it_is_and_named() {
    let world = World::start().await;
    world.run().await.unwrap();
    let shocks_before = world.versions(SHOCKS).await;
    let mut unreadable = shocks(Vec::new());
    unreadable.events = Err("the table is cut short".to_owned());
    let more_storms = list(
        "storms",
        "cat-storms",
        vec![event("s1", 10), event("s2", 11)],
    );
    world.answer(Ok(vec![unreadable, more_storms]));

    let report = world.run().await.unwrap();

    assert_eq!(report.unreadable, ["Fake shocks: the table is cut short"]);
    assert_eq!(report.unpublished, 0);
    assert_eq!(report.counts.added, 1);
    assert_eq!(world.listed(SHOCKS, "cat-shocks").await, ["e1", "e2"]);
    assert_eq!(world.versions(SHOCKS).await, shocks_before);
    assert_eq!(world.row(SHOCKS).await["relay"]["published"], true);
    assert_eq!(world.listed(STORMS, "cat-storms").await, ["s1", "s2"]);
    let provider = store::provider(&world.state.pool, "fake")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(provider.last_run_at, provider.last_ok_at, "the run worked");
    let result: Value = serde_json::from_str(&provider.last_result.unwrap()).unwrap();
    assert_eq!(
        result["unreadable"],
        json!(["Fake shocks: the table is cut short"])
    );
}

#[tokio::test]
async fn a_list_never_read_is_not_made() {
    let world = World::start().await;
    let mut unreadable = shocks(Vec::new());
    unreadable.events = Err("no answer".to_owned());
    world.answer(Ok(vec![unreadable, storms()]));

    let report = world.run().await.unwrap();

    assert_eq!(report.unreadable, ["Fake shocks: no answer"]);
    let index = world.index(world.reader).await;
    assert!(index.iter().all(|row| row["id"] != SHOCKS), "{index:?}");
}

#[tokio::test]
async fn a_list_the_provider_stops_publishing_stays_marked() {
    let world = World::start().await;
    world.run().await.unwrap();
    let refreshed = world.row(STORMS).await["relay"]["refreshed_at"].clone();
    world.answer(Ok(vec![shocks(vec![event("e1", 1)])]));

    let report = world.run().await.unwrap();

    assert_eq!(report.catalogues, 1);
    assert_eq!(report.unpublished, 1);
    let row = world.row(STORMS).await;
    assert_eq!(row["relay"]["published"], false);
    assert_eq!(row["relay"]["refreshed_at"], refreshed);
    assert_eq!(row["count"], 1);
    assert_eq!(world.row(SHOCKS).await["relay"]["published"], true);
}

#[tokio::test]
async fn a_provider_never_runs_twice_at_once() {
    let (started, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let world = World::with_gate(Some((started.clone(), release.clone()))).await;
    let state = world.state.clone();
    let first = tokio::spawn(async move { relay::run(&state, "fake").await });
    tokio::time::timeout(Duration::from_secs(10), started.notified())
        .await
        .expect("the first run never fetched");

    let second = world.run().await;

    assert!(matches!(second, Err(RelayError::Running)), "{second:?}");
    release.notify_one();
    first.await.unwrap().unwrap();
    release.notify_one();
    world.run().await.unwrap();
}

#[tokio::test]
async fn an_unknown_provider_is_refused() {
    let world = World::start().await;

    let run = relay::run(&world.state, "nobody").await;

    assert!(matches!(run, Err(RelayError::Unknown(_))), "{run:?}");
}

#[tokio::test]
async fn nobody_edits_a_relayed_catalogue() {
    let world = World::start().await;
    world.public_before_the_relay().await;
    let path = format!("/api/public/{SHOCKS}/proposals");
    world
        .state
        .rooms
        .write_as(SHOCKS, u64::MAX, Author::Client(world.curator), seeded())
        .await
        .unwrap();
    let (status, copied) = world
        .call(world.author, Method::POST, &path, Some(json!({})))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{copied}");
    let copy = copied["room"].as_str().unwrap().to_owned();
    let update = json!({"room": copy, "catalogue": "cat-shocks"});
    let (status, pending) = world
        .call(
            world.author,
            Method::POST,
            "/api/public/submissions/updates",
            Some(update.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{pending}");

    world.run().await.unwrap();

    let refused = |(status, body): (StatusCode, Value)| {
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["detail"], FOLLOWS);
    };
    let room = format!("/api/rooms/{SHOCKS}");
    let public = format!("/api/public/{SHOCKS}");
    refused(
        world
            .call(
                world.curator,
                Method::PATCH,
                &public,
                Some(json!({"name": "Mine"})),
            )
            .await,
    );
    refused(world.call(world.curator, Method::DELETE, &room, None).await);
    refused(
        world
            .call(world.author, Method::POST, &path, Some(json!({})))
            .await,
    );
    refused(
        world
            .call(
                world.author,
                Method::POST,
                "/api/public/submissions/updates",
                Some(update),
            )
            .await,
    );
    let review = format!("/api/public/submissions/{}/review", pending["id"]);
    refused(
        world
            .call(
                world.curator,
                Method::POST,
                &review,
                Some(json!({"accept": true})),
            )
            .await,
    );
    let (_, detail) = world
        .call(
            world.curator,
            Method::GET,
            &format!("/api/public/submissions/{}", pending["id"]),
            None,
        )
        .await;
    assert_eq!(detail["status"], "pending", "the refusal decided nothing");
    let (status, _) = world
        .call(
            world.reader,
            Method::POST,
            &format!("/api/public/{SHOCKS}/copies"),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "copying stays open");
    assert_eq!(world.socket_role(world.curator).await, "viewer");
}

impl World {
    /// `public.fake.shocks` as a curator made it before the relay followed it, the way the
    /// AMDA script made AMDA's.
    async fn public_before_the_relay(&self) {
        let room = db::PublicRoom {
            id: SHOCKS.to_owned(),
            name: "Shocks".to_owned(),
            folder: None,
            source: None,
            description: None,
            public_count: None,
            counted_file: None,
        };
        assert!(
            db::create_public_room(&self.state.pool, &room, Some(self.curator))
                .await
                .unwrap()
        );
    }

    /// The role the socket handshake grants `user` on the relayed catalogue.
    async fn socket_role(&self, user: Uuid) -> String {
        let (role, _socket, server) = self.socket(user).await;
        server.abort();
        role
    }

    /// A socket of `user` on `public.fake.shocks`, the role it was granted, and the server
    /// answering it.
    async fn socket(&self, user: Uuid) -> (String, Socket, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = ws::router(self.state.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut request = format!("ws://{address}/room/{SHOCKS}")
            .into_client_request()
            .unwrap();
        let token = auth::start_session(&self.state.pool, user).await.unwrap();
        let headers = request.headers_mut();
        headers.insert(
            "sec-websocket-protocol",
            cocat_core::SUBPROTOCOL.parse().unwrap(),
        );
        headers.insert(
            header::COOKIE,
            format!("fastapiusersauth={token}").parse().unwrap(),
        );
        let (socket, answer) = connect_async(request).await.unwrap();
        let role = answer.headers()[cocat_core::ROLE_HEADER]
            .to_str()
            .unwrap()
            .to_owned();
        (role, socket, server)
    }
}

/// The catalogue the fake provider calls shocks, as a curator wrote it before the relay.
fn seeded() -> Vec<u8> {
    let doc = Doc::new();
    let mut txn = doc.transact_mut();
    model::create_event(&mut txn, &event("e1", 1)).unwrap();
    let new = model::NewCatalogue {
        uuid: "cat-shocks".to_owned(),
        name: "Shocks".to_owned(),
        ..model::NewCatalogue::default()
    };
    model::create_catalogue(&mut txn, &new).unwrap();
    assert!(model::add_catalogue_events(
        &mut txn,
        "cat-shocks",
        &["e1".to_owned()]
    ));
    txn.encode_update_v1()
}

#[tokio::test]
async fn the_public_index_says_where_a_relayed_catalogue_comes_from() {
    let world = World::start().await;
    let (status, plain) = world
        .call(
            world.curator,
            Method::POST,
            "/api/public",
            Some(json!({"name": "Hand made"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{plain}");
    world.run().await.unwrap();

    let index = world.index(world.curator).await;

    let row = |id: &Value| index.iter().find(|row| row["id"] == *id).unwrap().clone();
    let relayed = row(&json!(SHOCKS));
    assert_eq!(relayed["relay"]["source"], SOURCE);
    assert_eq!(relayed["relay"]["published"], true);
    assert_eq!(relayed["relay"]["url"], "https://fake.example.org/shocks");
    assert!(relayed["relay"]["refreshed_at"].is_string(), "{relayed}");
    assert_eq!(relayed["role"], "viewer", "a curator only reads it");
    let plain = row(&plain["id"]);
    assert_eq!(plain["relay"], Value::Null);
    assert_eq!(plain["role"], "editor");
}

#[tokio::test]
async fn a_panicking_provider_is_recorded_failed_and_runs_again() {
    let world = World::start().await;
    world.answer(Err(PANIC.to_owned()));

    let failed = world.run().await;

    assert!(matches!(failed, Err(RelayError::Panicked(_))), "{failed:?}");
    let provider = store::provider(&world.state.pool, "fake")
        .await
        .unwrap()
        .unwrap();
    assert!(!provider.running);
    assert!(provider.last_ok_at.is_none());
    assert!(provider.last_result.unwrap().contains("error"));
    world.answer(Ok(two_lists()));
    world.run().await.unwrap();
}

#[tokio::test]
async fn a_run_whose_caller_gave_up_still_ends_and_clears_running() {
    let (started, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let world = World::with_gate(Some((started.clone(), release.clone()))).await;
    let state = world.state.clone();
    let caller = tokio::spawn(async move { relay::run(&state, "fake").await });
    tokio::time::timeout(Duration::from_secs(10), started.notified())
        .await
        .expect("the run never fetched");

    caller.abort();
    let _ = caller.await;
    release.notify_one();

    // simplify: nothing signals the end of a run nobody awaits, so this polls the row.
    tokio::time::timeout(Duration::from_secs(10), async {
        while store::provider(&world.state.pool, "fake")
            .await
            .unwrap()
            .unwrap()
            .running
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the run stayed marked running");
    assert_eq!(
        world.row(SHOCKS).await["count"],
        2,
        "the run went to its end"
    );
}

#[tokio::test]
async fn a_curator_editing_when_the_relay_adopts_the_catalogue_is_cut_off() {
    let world = World::start().await;
    world.public_before_the_relay().await;
    let (role, mut socket, server) = world.socket(world.curator).await;
    assert_eq!(role, "editor");
    socket.next().await.unwrap().unwrap();

    world.run().await.unwrap();

    let close = loop {
        match tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("the socket stayed open")
        {
            Some(Ok(WsMessage::Close(frame))) => break frame,
            Some(Ok(_)) => continue,
            other => panic!("expected a close, got {other:?}"),
        }
    };
    server.abort();
    assert_eq!(
        close.map(|frame| u16::from(frame.code)),
        Some(cocat_core::ACCESS_CHANGED_CLOSE)
    );
}

#[tokio::test]
async fn nobody_takes_a_providers_ids_by_hand() {
    let world = World::start().await;

    let (status, refused) = world
        .call(
            world.curator,
            Method::POST,
            "/api/public",
            Some(json!({"name": "Mine", "key": "fake.mine"})),
        )
        .await;
    let (other, _) = world
        .call(
            world.curator,
            Method::POST,
            "/api/public",
            Some(json!({"name": "Mine", "key": "fakery.mine"})),
        )
        .await;

    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(
        refused["detail"],
        "Ids starting with public.fake. belong to the Fake source catalogues"
    );
    assert_eq!(other, StatusCode::CREATED);
}

#[tokio::test]
async fn a_bad_key_fails_the_run_before_anything_is_written() {
    let world = World::start().await;
    world.answer(Ok(vec![
        shocks(vec![event("e1", 1)]),
        list("not a key", "cat-bad", vec![event("b1", 1)]),
    ]));

    let failed = world.run().await;

    assert!(matches!(failed, Err(RelayError::Key(_))), "{failed:?}");
    assert!(world.index(world.reader).await.is_empty());
}
