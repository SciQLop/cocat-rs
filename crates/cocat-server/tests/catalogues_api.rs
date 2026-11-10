use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use bytes::Bytes;
use cocat_core::model::{self, EventRecord, NewCatalogue};
use cocat_core::role::Role;
use cocat_core::store::FileRoomStore;
use cocat_core::time::Timestamp;
use cocat_server::room::RoomManager;
use cocat_server::{AppState, auth, db, links::Links, ratelimit::LoginLimit, ws};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;
use yrs::{Doc, ReadTxn, StateVector, Transact, TransactionMut};

const ROOM: &str = "room-read";
const ALPHA: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const BETA: &str = "bbbbbbbb-0000-4000-8000-000000000002";
const MIXED: &str = "cccccccc-0000-4000-8000-000000000003";
const PAGED: &str = "dddddddd-0000-4000-8000-000000000004";
const E1: &str = "11111111-0000-4000-8000-000000000001";
const E2: &str = "22222222-0000-4000-8000-000000000002";
const E3: &str = "33333333-0000-4000-8000-000000000003";
const M1: &str = "44444444-0000-4000-8000-000000000001";
const M2: &str = "44444444-0000-4000-8000-000000000002";

struct Fixture {
    state: AppState,
    update_dir: PathBuf,
    _dir: TempDir,
}

impl Fixture {
    async fn start() -> Self {
        Self::start_with_budget(None).await
    }

    async fn start_with_budget(budget: Option<u64>) -> Self {
        let dir = TempDir::new().unwrap();
        let update_dir = dir.path().join("rooms");
        fs::create_dir_all(&update_dir).unwrap();
        let pool = db::open(&dir.path().join("test.db")).await.unwrap();
        let state = AppState {
            pool,
            secret: "test-secret".to_owned(),
            rooms: RoomManager::new(update_dir.clone(), budget),
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
            update_dir,
            _dir: dir,
        }
    }

    async fn user(&self, email: &str) -> Uuid {
        db::create_user(&self.state.pool, email, "hunter2", false, true)
            .await
            .unwrap()
    }

    async fn token(&self, user: Uuid) -> String {
        auth::start_session(&self.state.pool, user).await.unwrap()
    }

    async fn room(&self, owner: Uuid) {
        db::create_room(
            &self.state.pool,
            owner,
            ROOM,
            "Reading room",
            db::ProjectKind::Catalogues,
        )
        .await
        .unwrap();
    }

    fn seed(&self, room_id: &str, doc: &Doc) {
        let update = doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        let (mut store, _) = FileRoomStore::open(&self.update_dir, room_id).unwrap();
        store.append(Bytes::from(update)).unwrap();
        store.flush().unwrap();
    }

    fn request(&self, path: &str, token: Option<&str>) -> Request<Body> {
        let mut request = Request::builder().method(Method::GET).uri(path);
        if let Some(token) = token {
            request = request.header(header::COOKIE, format!("fastapiusersauth={token}"));
        }
        request.body(Body::empty()).unwrap()
    }

    async fn raw(&self, path: &str, token: Option<&str>) -> Response {
        self.router()
            .oneshot(self.request(path, token))
            .await
            .unwrap()
    }

    fn router(&self) -> Router {
        ws::router(self.state.clone())
    }

    async fn get(&self, path: &str, token: Option<&str>) -> (StatusCode, Value) {
        let response = self.raw(path, token).await;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("the response is not JSON")
        };
        (status, body)
    }

    fn open_rooms(&self) -> usize {
        self.state.rooms.open_rooms()
    }
}

fn object(value: Value) -> serde_json::Map<String, Value> {
    value.as_object().cloned().unwrap()
}

fn event(
    uuid: &str,
    start: &str,
    confidence: Option<f64>,
    tags: &[&str],
    attributes: Value,
) -> EventRecord {
    let start = Timestamp::parse(start).unwrap();
    let mut event = EventRecord::new(uuid, start, start);
    event.author = Some("a@x.org".to_owned());
    event.tags = tags.iter().map(|tag| (*tag).to_owned()).collect();
    event.products = vec!["p1".to_owned()];
    event.confidence = confidence;
    event.attributes = object(attributes);
    event
}

fn catalogue(
    txn: &mut TransactionMut<'_>,
    uuid: &str,
    name: &str,
    events: &[&str],
    attributes: Value,
) {
    let catalogue = NewCatalogue {
        uuid: uuid.to_owned(),
        name: name.to_owned(),
        author: Some("a@x.org".to_owned()),
        tags: vec!["t1".to_owned()],
        attributes: object(attributes),
        ..NewCatalogue::default()
    };
    model::create_catalogue(txn, &catalogue).unwrap();
    let events: Vec<String> = events.iter().map(|event| (*event).to_owned()).collect();
    model::add_catalogue_events(txn, uuid, &events);
}

fn library() -> Doc {
    let doc = Doc::new();
    {
        let mut txn = doc.transact_mut();
        let shared = json!({"mission": "MMS", "level": 2});
        catalogue(&mut txn, ALPHA, "alpha", &[E1, E2, E3], shared.clone());
        catalogue(&mut txn, BETA, "beta", &[], json!({}));
        catalogue(&mut txn, MIXED, "mixed", &[M1, M2], json!({}));
        let events = [
            event(
                E1,
                "2025-01-31 10:00:00",
                Some(0.3),
                &["t1", "a<b", "x&y"],
                shared.clone(),
            ),
            event(
                E2,
                "2025-01-31 11:00:00.123456",
                None,
                &["t1"],
                shared.clone(),
            ),
            event(E3, "2025-01-31 12:00:00+00:00", Some(0.1), &["t1"], shared),
            event(M1, "2025-03-01 00:00:00", None, &[], json!({"x": 1})),
            event(M2, "2025-03-01 00:01:00", None, &[], json!({"x": "s"})),
        ];
        for event in &events {
            model::create_event(&mut txn, event).unwrap();
        }
    }
    doc
}

/// 25 events, ten of them sharing one start, so paging has to break the tie by uuid.
fn paged() -> Doc {
    let doc = Doc::new();
    {
        let mut txn = doc.transact_mut();
        let ids: Vec<String> = (0..25)
            .map(|index| format!("55555555-0000-4000-8000-{index:012}"))
            .collect();
        let borrowed: Vec<&str> = ids.iter().map(String::as_str).collect();
        catalogue(&mut txn, PAGED, "paged", &borrowed, json!({}));
        for (index, uuid) in ids.iter().enumerate() {
            let start = match index {
                15..=24 => "2025-02-01 00:10:00".to_owned(),
                index => format!("2025-02-01 00:{:02}:00", index),
            };
            model::create_event(&mut txn, &event(uuid, &start, None, &[], json!({}))).unwrap();
        }
    }
    doc
}

async fn fixture_with_library() -> (Fixture, Uuid) {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room(owner).await;
    fixture.seed(ROOM, &library());
    (fixture, owner)
}

#[tokio::test]
async fn lists_catalogues_for_a_viewer() {
    let (fixture, _owner) = fixture_with_library().await;
    let viewer = fixture.user("viewer@example.org").await;
    db::add_member(&fixture.state.pool, ROOM, viewer, Role::Viewer)
        .await
        .unwrap();

    let (status, body) = fixture
        .get(
            "/api/rooms/room-read/catalogues",
            Some(&fixture.token(viewer).await),
        )
        .await;

    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = body["catalogues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|catalogue| catalogue["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["alpha", "beta", "mixed"]);
    assert_eq!(body["catalogues"][0]["event_count"], 3);
    assert_eq!(body["catalogues"][0]["tags"], json!(["t1"]));
    assert_eq!(
        body["catalogues"][0]["attributes"],
        json!({"level": 2, "mission": "MMS"})
    );
}

#[tokio::test]
async fn non_member_gets_404() {
    let (fixture, _) = fixture_with_library().await;
    let stranger = fixture.user("stranger@example.org").await;

    let (status, body) = fixture
        .get(
            "/api/rooms/room-read/catalogues",
            Some(&fixture.token(stranger).await),
        )
        .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({"detail": "Project room-read not found"}));
}

#[tokio::test]
async fn anonymous_gets_401() {
    let (fixture, _) = fixture_with_library().await;

    let (status, body) = fixture.get("/api/rooms/room-read/catalogues", None).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, json!({"detail": "Unauthorized"}));
}

#[tokio::test]
async fn unknown_catalogue_404() {
    let (fixture, owner) = fixture_with_library().await;
    let token = fixture.token(owner).await;

    let (status, body) = fixture
        .get(
            "/api/rooms/room-read/catalogues/eeeeeeee-0000-4000-8000-000000000009",
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body,
        json!({"detail": "Catalogue eeeeeeee-0000-4000-8000-000000000009 not found"})
    );

    let (status, _) = fixture
        .get(
            "/api/rooms/room-read/catalogues/eeeeeeee-0000-4000-8000-000000000009/events",
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn one_catalogue() {
    let (fixture, owner) = fixture_with_library().await;

    let (status, body) = fixture
        .get(
            &format!("/api/rooms/room-read/catalogues/{ALPHA}"),
            Some(&fixture.token(owner).await),
        )
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["uuid"], ALPHA);
    assert_eq!(body["name"], "alpha");
    assert_eq!(body["author"], "a@x.org");
    assert_eq!(body["event_count"], 3);
}

#[tokio::test]
async fn events_are_sorted_and_iso_utc() {
    let (fixture, owner) = fixture_with_library().await;

    let (status, body) = fixture
        .get(
            &format!("/api/rooms/room-read/catalogues/{ALPHA}/events"),
            Some(&fixture.token(owner).await),
        )
        .await;

    assert_eq!(status, StatusCode::OK);
    let events = body["events"].as_array().unwrap();
    let uuids: Vec<&str> = events
        .iter()
        .map(|event| event["uuid"].as_str().unwrap())
        .collect();
    assert_eq!(uuids, [E1, E2, E3]);
    let starts: Vec<&str> = events
        .iter()
        .map(|event| event["start"].as_str().unwrap())
        .collect();
    assert_eq!(
        starts,
        [
            "2025-01-31T10:00:00.000000Z",
            "2025-01-31T11:00:00.123456Z",
            "2025-01-31T12:00:00.000000Z",
        ]
    );
    assert_eq!(events[0]["confidence"], 0.3);
    assert_eq!(events[1]["confidence"], Value::Null);
    assert_eq!(events[0]["tags"], json!(["a<b", "t1", "x&y"]));
    assert_eq!(body["next"], Value::Null);
}

#[tokio::test]
async fn time_range_keeps_overlapping_events() {
    let (fixture, owner) = fixture_with_library().await;

    let (status, body) = fixture
        .get(
            &format!(
                "/api/rooms/room-read/catalogues/{ALPHA}/events?start=2025-01-31T10:30:00Z&stop=2025-01-31T11:30:00Z"
            ),
            Some(&fixture.token(owner).await),
        )
        .await;

    assert_eq!(status, StatusCode::OK);
    let uuids: Vec<&str> = body["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["uuid"].as_str().unwrap())
        .collect();
    assert_eq!(uuids, [E2]);
}

#[tokio::test]
async fn start_after_stop_is_422() {
    let (fixture, owner) = fixture_with_library().await;

    let (status, body) = fixture
        .get(
            &format!(
                "/api/rooms/room-read/catalogues/{ALPHA}/events?start=2025-01-31T12:00:00Z&stop=2025-01-31T11:00:00Z"
            ),
            Some(&fixture.token(owner).await),
        )
        .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["detail"], "the start must not be after the stop");

    let (status, _) = fixture
        .get(
            &format!("/api/rooms/room-read/catalogues/{ALPHA}/events?start=not-a-time"),
            Some(&fixture.token(owner).await),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn limit_out_of_range_is_422() {
    let (fixture, owner) = fixture_with_library().await;
    let token = fixture.token(owner).await;

    for limit in ["0", "10001", "many"] {
        let (status, _) = fixture
            .get(
                &format!("/api/rooms/room-read/catalogues/{ALPHA}/events?limit={limit}"),
                Some(&token),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "limit {limit} was accepted"
        );
    }
}

#[tokio::test]
async fn paging_with_equal_starts_returns_each_event_once() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room(owner).await;
    fixture.seed(ROOM, &paged());
    let token = fixture.token(owner).await;

    let (status, unpaged) = fixture
        .get(
            &format!("/api/rooms/room-read/catalogues/{PAGED}/events"),
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let expected: Vec<&str> = unpaged["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["uuid"].as_str().unwrap())
        .collect();
    assert_eq!(expected.len(), 25);

    let mut seen: Vec<String> = Vec::new();
    let mut path = format!("/api/rooms/room-read/catalogues/{PAGED}/events?limit=7");
    loop {
        let (status, body) = fixture.get(&path, Some(&token)).await;
        assert_eq!(status, StatusCode::OK);
        let events = body["events"].as_array().unwrap();
        assert!(events.len() <= 7);
        for event in events {
            seen.push(event["uuid"].as_str().unwrap().to_owned());
        }
        match body["next"].as_str() {
            Some(next) => {
                path =
                    format!("/api/rooms/room-read/catalogues/{PAGED}/events?limit=7&cursor={next}");
                assert!(seen.len() < 25, "the cursor did not advance");
            }
            None => break,
        }
    }
    let seen: Vec<&str> = seen.iter().map(String::as_str).collect();
    assert_eq!(seen, expected);
}

#[tokio::test]
async fn bad_cursor_is_422() {
    let (fixture, owner) = fixture_with_library().await;
    let token = fixture.token(owner).await;

    for cursor in ["!!!", "Z2FyYmFnZQ"] {
        let (status, _) = fixture
            .get(
                &format!("/api/rooms/room-read/catalogues/{ALPHA}/events?cursor={cursor}"),
                Some(&token),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "cursor {cursor} was accepted"
        );
    }
}

#[tokio::test]
async fn votable_suffix_returns_xml() {
    let (fixture, owner) = fixture_with_library().await;

    let response = fixture
        .raw(
            &format!("/api/rooms/room-read/catalogues/{ALPHA}.votable"),
            Some(&fixture.token(owner).await),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/x-votable+xml"
    );
    assert_eq!(
        response.headers()[header::CONTENT_DISPOSITION],
        format!("attachment; filename=\"{ALPHA}.xml\"")
    );
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>"));
    assert!(body.contains("<VOTABLE"));
    assert!(body.contains("<TD>&quot;a&lt;b&quot;</TD>") || body.contains("a&lt;b"));
    assert!(body.contains("<TD>0.3</TD>"));
}

#[tokio::test]
async fn votable_mixed_types_is_422() {
    let (fixture, owner) = fixture_with_library().await;

    let (status, body) = fixture
        .get(
            &format!("/api/rooms/room-read/catalogues/{MIXED}.votable"),
            Some(&fixture.token(owner).await),
        )
        .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["detail"]
            .as_str()
            .unwrap()
            .contains("not all value types are identical for all events for attribute x"),
        "{body}"
    );
}

#[tokio::test]
async fn rest_read_does_not_open_the_room() {
    let (fixture, owner) = fixture_with_library().await;
    let token = fixture.token(owner).await;

    let paths = [
        "/api/rooms/room-read/catalogues".to_owned(),
        format!("/api/rooms/room-read/catalogues/{ALPHA}"),
        format!("/api/rooms/room-read/catalogues/{ALPHA}/events?limit=2"),
        format!("/api/rooms/room-read/catalogues/{ALPHA}.votable"),
    ];
    for path in &paths {
        let response = fixture.raw(path, Some(&token)).await;
        assert!(
            response.status().is_success(),
            "{path} answered {}",
            response.status()
        );
        assert_eq!(fixture.open_rooms(), 0, "{path} opened the room");
    }
}

#[tokio::test]
async fn over_budget_is_503() {
    let fixture = Fixture::start_with_budget(Some(1)).await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room(owner).await;
    fixture.seed(ROOM, &library());

    let (status, body) = fixture
        .get(
            "/api/rooms/room-read/catalogues",
            Some(&fixture.token(owner).await),
        )
        .await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, json!({"detail": "Server memory budget reached"}));
    assert_eq!(fixture.open_rooms(), 0);
}
