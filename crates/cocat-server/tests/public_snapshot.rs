//! `/api/public/snapshot`: the public catalogues as Parquet files that a browser queries with
//! SQL (docs/design/browser-sql-analysis.md). Public data only, published fields only.

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use bytes::Bytes;
use cocat_core::model::{self, EventRecord};
use cocat_core::role::Role;
use cocat_core::store::FileRoomStore;
use cocat_core::time::Timestamp;
use cocat_server::room::RoomManager;
use cocat_server::{AppState, auth, db, links::Links, public_snapshot, ratelimit::LoginLimit, ws};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::file::reader::{FileReader, SerializedFileReader};
use serde_json::{Map, Value, json};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;
use yrs::Transact;

const EVENT_COLUMNS: [&str; 10] = [
    "event",
    "catalogue",
    "start",
    "stop",
    "status",
    "confidence",
    "start_err",
    "stop_err",
    "tags",
    "observer_raw",
];
const CATALOGUE_COLUMNS: [&str; 11] = [
    "catalogue",
    "name",
    "folder",
    "source",
    "description",
    "relay_source",
    "relay_url",
    "relay_published",
    "events",
    "attributes",
    "attributes_file",
];

struct World {
    state: AppState,
    update_dir: PathBuf,
    reader: Uuid,
    curator: Uuid,
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
            providers: Default::default(),
            http: Default::default(),
        };
        let reader = db::create_user(&state.pool, "reader@example.org", "pwd", false, true)
            .await
            .unwrap();
        let curator = db::create_user(&state.pool, "curator@example.org", "pwd", false, true)
            .await
            .unwrap();
        db::set_curator(&state.pool, curator, true).await.unwrap();
        Self {
            state,
            update_dir,
            reader,
            curator,
            _dir: dir,
        }
    }

    async fn public(&self, body: Value) -> String {
        let (status, created) = self
            .json(Some(self.curator), Method::POST, "/api/public", Some(body))
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
    ) -> (StatusCode, Bytes) {
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
        (
            status,
            to_bytes(response.into_body(), 1 << 26).await.unwrap(),
        )
    }

    async fn json(
        &self,
        caller: Option<Uuid>,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let (status, bytes) = self.call(caller, method, path, body).await;
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// Events written straight into `room`'s file: (uuid, start, author, attributes).
    fn append(&self, room: &str, events: &[(&str, &str, Option<&str>, Value)]) {
        let doc = yrs::Doc::new();
        let update = {
            let mut txn = doc.transact_mut();
            for (uuid, start, author, attributes) in events {
                let at = Timestamp::parse(start).unwrap();
                let mut record = EventRecord::new(*uuid, at, at);
                record.author = author.map(str::to_owned);
                record.attributes = attributes.as_object().cloned().unwrap_or_default();
                model::create_event(&mut txn, &record).unwrap();
            }
            txn.encode_update_v1()
        };
        let (mut store, _) = FileRoomStore::open(&self.update_dir, room).unwrap();
        store.append(Bytes::from(update)).unwrap();
        store.flush().unwrap();
    }

    async fn manifest(&self) -> Value {
        let (status, manifest) = self
            .json(Some(self.reader), Method::GET, "/api/public/snapshot", None)
            .await;
        assert_eq!(status, StatusCode::OK, "{manifest}");
        manifest
    }

    /// Every file the manifest names, by its name in the manifest.
    async fn files(&self, manifest: &Value) -> Vec<(String, Bytes)> {
        let mut files = Vec::new();
        for (logical, file) in manifest["files"].as_object().unwrap() {
            let path = format!("/api/public/snapshot/{}", file["name"].as_str().unwrap());
            let (status, bytes) = self.call(Some(self.reader), Method::GET, &path, None).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert_eq!(bytes.len() as u64, file["bytes"].as_u64().unwrap());
            files.push((logical.clone(), bytes));
        }
        files
    }
}

fn columns(bytes: &Bytes) -> Vec<String> {
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes.clone()).unwrap();
    builder
        .schema()
        .fields()
        .iter()
        .map(|field| field.name().clone())
        .collect()
}

fn rows(bytes: &Bytes) -> Vec<Map<String, Value>> {
    let reader = SerializedFileReader::new(bytes.clone()).unwrap();
    reader
        .get_row_iter(None)
        .unwrap()
        .map(|row| match row.unwrap().to_json_value() {
            Value::Object(row) => row,
            other => panic!("a row is an object, not {other}"),
        })
        .collect()
}

/// A private room, a shared room and a public catalogue, each with events by people.
async fn mixed_world() -> (World, String, Vec<String>) {
    let world = World::start().await;
    db::create_room(
        &world.state.pool,
        world.reader,
        "private-room-7f3a",
        "Private",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    world.append(
        "private-room-7f3a",
        &[(
            "p1",
            "2021-01-01T00:00:00Z",
            Some("secret@example.org"),
            json!({"contact": "secret@example.org"}),
        )],
    );
    db::create_room(
        &world.state.pool,
        world.reader,
        "shared-room-9c1b",
        "Shared",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    db::add_member(
        &world.state.pool,
        "shared-room-9c1b",
        world.curator,
        Role::Editor,
    )
    .await
    .unwrap();
    world.append(
        "shared-room-9c1b",
        &[(
            "s1",
            "2021-01-02T00:00:00Z",
            Some("reader@example.org"),
            json!({"x": 1}),
        )],
    );
    let shocks = world
        .public(json!({"name": "Shocks", "folder": "Literature/Earth", "source": "Paper"}))
        .await;
    world.append(
        &shocks,
        &[
            (
                "e2",
                "2020-01-02T00:00:00Z",
                Some("curator@example.org"),
                json!({"spacecraft": "Wind", "speed": 450, "quality": true}),
            ),
            (
                "e1",
                "2020-01-01T00:00:00Z",
                Some("curator@example.org"),
                json!({"spacecraft": "ACE", "speed": 380.5, "quality": "good"}),
            ),
        ],
    );
    let empty = world.public(json!({"name": "Nothing yet"})).await;
    (world, shocks, vec![empty])
}

#[tokio::test]
async fn the_snapshot_holds_public_data_and_published_fields_only() {
    let (world, shocks, _) = mixed_world().await;
    let manifest = world.manifest().await;
    let files = world.files(&manifest).await;
    assert!(files.iter().any(|(name, _)| name == "events.parquet"));
    assert!(files.iter().any(|(name, _)| name == "catalogues.parquet"));
    let attributes = format!("attributes/{shocks}.parquet");
    assert!(files.iter().any(|(name, _)| *name == attributes));
    assert_eq!(
        files
            .iter()
            .filter(|(name, _)| name.starts_with("attributes/"))
            .count(),
        1,
        "one catalogue has attributes"
    );

    for (name, bytes) in &files {
        let raw = String::from_utf8_lossy(bytes);
        let decoded = if name.ends_with(".parquet") {
            serde_json::to_string(&rows(bytes)).unwrap()
        } else {
            raw.to_string()
        };
        for text in [raw.as_ref(), decoded.as_str()] {
            assert!(!text.contains("example.org"), "{name} holds an address");
            assert!(
                !text.contains("private-room-7f3a"),
                "{name} names a private room"
            );
            assert!(
                !text.contains("shared-room-9c1b"),
                "{name} names a shared room"
            );
            assert!(!text.contains("secret"), "{name} holds a private attribute");
        }
        let found = if name.ends_with(".parquet") {
            columns(bytes)
        } else {
            continue;
        };
        let allowed: Vec<&str> = match name.split_once('.').map_or("", |(stem, _)| stem) {
            "events" => EVENT_COLUMNS.to_vec(),
            "catalogues" => CATALOGUE_COLUMNS.to_vec(),
            _ => vec!["_event", "_uuid", "quality", "spacecraft", "speed"],
        };
        assert_eq!(found, allowed, "{name}");
    }
}

#[tokio::test]
async fn events_and_their_attributes_join_on_the_event_number() {
    let (world, shocks, _) = mixed_world().await;
    let manifest = world.manifest().await;
    let files = world.files(&manifest).await;
    let file = |name: &str| rows(&files.iter().find(|(n, _)| n == name).unwrap().1);

    let events = file("events.parquet");
    assert_eq!(events.len(), 2, "public events only");
    assert_eq!(events[0]["catalogue"], shocks.as_str());
    assert_eq!(
        events[0]["observer_raw"], "ACE",
        "sorted by start, spacecraft pulled out"
    );
    assert_eq!(events[1]["observer_raw"], "Wind");

    let attributes = file(&format!("attributes/{shocks}.parquet"));
    let by_event: Vec<_> = attributes
        .iter()
        .map(|row| (row["_event"].clone(), row["_uuid"].clone()))
        .collect();
    assert_eq!(
        by_event,
        [
            (events[0]["event"].clone(), json!("e1")),
            (events[1]["event"].clone(), json!("e2"))
        ]
    );
    assert_eq!(
        attributes[1]["speed"],
        json!(450.0),
        "a number is always a double"
    );
    assert_eq!(attributes[0]["quality"], "good", "mixed types become text");
    assert_eq!(attributes[1]["quality"], "true");

    let catalogues = file("catalogues.parquet");
    assert_eq!(catalogues.len(), 2);
    let listed = catalogues
        .iter()
        .find(|row| row["catalogue"] == shocks.as_str())
        .unwrap();
    assert_eq!(listed["name"], "Shocks");
    assert_eq!(listed["folder"], "Literature/Earth");
    assert_eq!(listed["events"], 2);
    assert_eq!(
        listed["attributes_file"],
        manifest["files"][format!("attributes/{shocks}.parquet")]["name"]
    );
}

#[tokio::test]
async fn the_snapshot_is_parquet_only_with_numbers_and_times_as_deltas() {
    let (world, shocks, _) = mixed_world().await;
    let manifest = world.manifest().await;
    let names: Vec<&String> = manifest["files"].as_object().unwrap().keys().collect();
    assert!(
        names
            .iter()
            .all(|name| name.ends_with(".parquet") || *name == "survey.sql"),
        "{names:?}"
    );
    let files = world.files(&manifest).await;
    let file = |name: &str| &files.iter().find(|(n, _)| n == name).unwrap().1;
    let delta = |name: &str| -> Vec<String> {
        let metadata =
            parquet::file::serialized_reader::SerializedFileReader::new(file(name).clone())
                .unwrap()
                .metadata()
                .clone();
        metadata.row_groups()[0]
            .columns()
            .iter()
            .filter(|column| {
                column
                    .encodings()
                    .any(|encoding| encoding == parquet::basic::Encoding::DELTA_BINARY_PACKED)
            })
            .map(|column| column.column_path().string())
            .collect()
    };
    assert_eq!(delta("events.parquet"), ["event", "start", "stop"]);
    assert_eq!(delta(&format!("attributes/{shocks}.parquet")), ["_event"]);
}

#[tokio::test]
async fn an_attribute_named_like_an_event_column_keeps_its_own_type() {
    let (world, shocks, _) = mixed_world().await;
    let first = world.manifest().await;
    let attributes = json!({"start": 1.5, "stop": 2.5, "event": 3.5});
    world.append(&shocks, &[("e9", "2020-01-09T00:00:00Z", None, attributes)]);
    let mut seen = first.clone();
    for _ in 0..100 {
        seen = world.manifest().await;
        if seen["version"] != first["version"] {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_ne!(
        seen["version"], first["version"],
        "the snapshot was rewritten"
    );
    let files = world.files(&seen).await;
    let name = format!("attributes/{shocks}.parquet");
    let file = &files.iter().find(|(n, _)| *n == name).unwrap().1;
    let row = rows(file)
        .into_iter()
        .find(|row| row["_uuid"] == "e9")
        .unwrap();
    assert_eq!(
        (&row["start"], &row["stop"], &row["event"]),
        (&json!(1.5), &json!(2.5), &json!(3.5))
    );
}

#[tokio::test]
async fn a_change_reaches_the_manifest_soon_after_without_waiting_for_the_hour() {
    let (world, shocks, _) = mixed_world().await;
    let first = world.manifest().await;
    world.append(&shocks, &[("e9", "2020-01-09T00:00:00Z", None, json!({}))]);
    let mut seen = first.clone();
    for _ in 0..100 {
        seen = world.manifest().await;
        if seen["version"] != first["version"] {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_ne!(
        seen["version"], first["version"],
        "a request starts the rewrite"
    );
    let files = world.files(&seen).await;
    let events = files
        .iter()
        .find(|(name, _)| name == "events.parquet")
        .unwrap();
    assert_eq!(rows(&events.1).len(), 3);
}

#[tokio::test]
async fn the_snapshot_carries_the_survey_method() {
    let (world, _, _) = mixed_world().await;
    let manifest = world.manifest().await;
    let files = world.files(&manifest).await;
    let (_, survey) = files.iter().find(|(name, _)| name == "survey.sql").unwrap();
    assert_eq!(survey.as_ref(), include_bytes!("../sql/survey.sql"));
    assert!(
        manifest["files"]["survey.sql"]["name"]
            .as_str()
            .unwrap()
            .ends_with(".sql")
    );
}

#[tokio::test]
async fn the_snapshot_needs_an_account_and_serves_only_its_own_files() {
    let (world, _, _) = mixed_world().await;
    let (status, _) = world
        .call(None, Method::GET, "/api/public/snapshot", None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let manifest = world.manifest().await;
    let name = manifest["files"]["events.parquet"]["name"]
        .as_str()
        .unwrap();
    let (status, _) = world
        .call(
            None,
            Method::GET,
            &format!("/api/public/snapshot/{name}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    for wrong in ["nothing.parquet", "..%2Ftest.db", "..", "events.parquet"] {
        let (status, _) = world
            .call(
                Some(world.reader),
                Method::GET,
                &format!("/api/public/snapshot/{wrong}"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{wrong}");
    }
}

#[tokio::test]
async fn a_change_makes_new_files_and_the_last_ones_stay_readable() {
    let (world, shocks, _) = mixed_world().await;
    let first = world.manifest().await;
    public_snapshot::refresh(&world.state).await.unwrap();
    assert_eq!(
        world.manifest().await,
        first,
        "nothing changed, nothing rewritten"
    );

    world.append(&shocks, &[("e3", "2020-01-03T00:00:00Z", None, json!({}))]);
    public_snapshot::refresh(&world.state).await.unwrap();
    let second = world.manifest().await;
    assert_ne!(second["version"], first["version"]);
    assert_ne!(
        second["files"]["events.parquet"]["name"],
        first["files"]["events.parquet"]["name"]
    );
    world.files(&first).await;
    world.files(&second).await;
}
