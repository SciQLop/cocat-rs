use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use bytes::Bytes;
use cocat_core::model::{self, EventField, EventRecord, NewCatalogue};
use cocat_core::role::Role;
use cocat_core::store::FileRoomStore;
use cocat_core::time::Timestamp;
use cocat_server::history::stamps::{StampFile, stamp_for};
use cocat_server::room::{RoomManager, outbox};
use cocat_server::{AppState, auth, db, links::Links, presence, ratelimit::LoginLimit, ws};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;
use yrs::sync::{Message, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, ReadTxn, Transact, TransactionMut, Update};

const ROOM: &str = "history-room";
const MINUTE: i64 = 60_000;

fn now_ms() -> i64 {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    i64::try_from(since.as_millis()).unwrap()
}

struct Fixture {
    state: AppState,
    update_dir: PathBuf,
    /// The room as the seeded records leave it, to encode the next record against.
    doc: Doc,
    alice: Uuid,
    bob: Uuid,
    carol: Uuid,
    _dir: TempDir,
}

impl Fixture {
    /// Alice owns the room, Bob edits it and Carol views it.
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
        let user = |email: &'static str| {
            let pool = state.pool.clone();
            async move {
                db::create_user(&pool, email, "hunter2", false, true)
                    .await
                    .unwrap()
            }
        };
        let (alice, bob, carol) = (
            user("alice@x.org").await,
            user("bob@x.org").await,
            user("carol@x.org").await,
        );
        db::create_room(
            &state.pool,
            alice,
            ROOM,
            "History",
            db::ProjectKind::Catalogues,
        )
        .await
        .unwrap();
        db::add_member(&state.pool, ROOM, bob, Role::Editor)
            .await
            .unwrap();
        db::add_member(&state.pool, ROOM, carol, Role::Viewer)
            .await
            .unwrap();
        Self {
            state,
            update_dir,
            doc: Doc::new(),
            alice,
            bob,
            carol,
            _dir: dir,
        }
    }

    /// Appends one record to the room file, stamped with `user` at `at_ms`.
    fn write<T>(&self, user: Uuid, at_ms: i64, edit: impl FnOnce(&mut TransactionMut) -> T) {
        let record = self.record(edit);
        let (mut store, records) = FileRoomStore::open(&self.update_dir, ROOM).unwrap();
        let index = records.len() as u64;
        store.append(record.clone()).unwrap();
        store.flush().unwrap();
        let mut stamps = StampFile::open(&self.update_dir, ROOM).unwrap();
        stamps.append(stamp_for(index, Some(user), at_ms, &record));
        stamps.write().unwrap();
    }

    fn record<T>(&self, edit: impl FnOnce(&mut TransactionMut) -> T) -> Bytes {
        let before = self.doc.transact().state_vector();
        let mut txn = self.doc.transact_mut();
        edit(&mut txn);
        Bytes::from(txn.encode_diff_v1(&before))
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        user: Uuid,
        body: Option<Value>,
    ) -> (StatusCode, Option<String>, Bytes) {
        let token = auth::start_session(&self.state.pool, user).await.unwrap();
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
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .map(|value| value.to_str().unwrap().to_owned());
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, content_type, bytes)
    }

    async fn get(&self, path: &str, user: Uuid) -> (StatusCode, Value) {
        let (status, _, bytes) = self.call(Method::GET, path, user, None).await;
        (status, json_of(&bytes))
    }

    async fn history(&self, user: Uuid) -> Vec<Value> {
        let (status, body) = self.get(&format!("/api/rooms/{ROOM}/history"), user).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["versions"].as_array().unwrap().clone()
    }

    async fn restore(&self, version: u64, user: Uuid) -> (StatusCode, Value) {
        let path = format!("/api/rooms/{ROOM}/history/{version}/restore");
        let body = Some(json!({ "catalogue": null }));
        let (status, _, bytes) = self.call(Method::POST, &path, user, body).await;
        (status, json_of(&bytes))
    }

    /// The room as of its newest version, as the state route serves it.
    async fn latest(&self) -> Doc {
        let newest = ids(&self.history(self.alice).await)[0];
        let path = format!("/api/rooms/{ROOM}/history/{newest}/state");
        let (status, _, bytes) = self.call(Method::GET, &path, self.alice, None).await;
        assert_eq!(status, StatusCode::OK);
        doc_of(&bytes)
    }

    async fn name(&self, version: u64, user: Uuid, name: &str) -> (StatusCode, Value) {
        let path = format!("/api/rooms/{ROOM}/history/{version}/name");
        let body = Some(json!({ "name": name }));
        let (status, _, bytes) = self.call(Method::PUT, &path, user, body).await;
        (status, json_of(&bytes))
    }
}

fn json_of(bytes: &[u8]) -> Value {
    if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(bytes).expect("the response is not JSON")
    }
}

fn doc_of(update: &[u8]) -> Doc {
    let doc = Doc::new();
    doc.transact_mut()
        .apply_update(Update::decode_v1(update).unwrap())
        .unwrap();
    doc
}

fn event(uuid: &str) -> EventRecord {
    let at = Timestamp::parse("2020-01-01T00:00:00Z").unwrap();
    EventRecord::new(uuid, at, at)
}

fn create(uuid: &'static str) -> impl FnOnce(&mut TransactionMut) {
    move |txn| {
        model::create_event(txn, &event(uuid)).unwrap();
    }
}

fn catalogue(uuid: &str) -> NewCatalogue {
    NewCatalogue {
        uuid: uuid.to_owned(),
        name: uuid.to_owned(),
        ..NewCatalogue::default()
    }
}

fn ids(versions: &[Value]) -> Vec<u64> {
    versions
        .iter()
        .map(|version| version["id"].as_u64().unwrap())
        .collect()
}

/// Alice adds `a`, then Bob changes `a` and adds `b`: versions 0 and 1.
async fn two_versions() -> Fixture {
    let fixture = Fixture::start().await;
    fixture.write(fixture.alice, 0, create("a"));
    fixture.write(fixture.bob, MINUTE, |txn| {
        model::set_event_field(txn, "a", &EventField::Confidence(Some(0.5))).unwrap();
        model::create_event(txn, &event("b")).unwrap();
    });
    fixture
}

#[tokio::test]
async fn members_list_versions_with_author_emails() {
    let fixture = two_versions().await;

    let versions = fixture.history(fixture.carol).await;

    assert_eq!(ids(&versions), [1, 0]);
    assert_eq!(versions[0]["author"], json!(fixture.bob));
    assert_eq!(versions[0]["author_email"], "bob@x.org");
    assert_eq!(versions[0]["events_added"], 1);
    assert_eq!(versions[0]["events_changed"], 1);
    assert_eq!(versions[0]["started_ms"], MINUTE);
    assert_eq!(versions[0]["name"], Value::Null);
    assert_eq!(versions[1]["author_email"], "alice@x.org");
}

#[tokio::test]
async fn an_author_has_the_colour_presence_gives_the_account() {
    let fixture = two_versions().await;

    let versions = fixture.history(fixture.carol).await;

    assert_eq!(versions[0]["author_color"], presence::color("bob@x.org"));
    assert_eq!(versions[1]["author_color"], presence::color("alice@x.org"));
}

#[tokio::test]
async fn an_event_filter_keeps_the_versions_that_touched_the_event() {
    let fixture = two_versions().await;
    fixture.write(fixture.alice, 2 * MINUTE, create("c"));
    let filtered = |event: &'static str| {
        let fixture = &fixture;
        async move {
            let path = format!("/api/rooms/{ROOM}/history?event={event}");
            let (status, body) = fixture.get(&path, fixture.carol).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            ids(body["versions"].as_array().unwrap())
        }
    };

    assert_eq!(filtered("a").await, [1, 0]);
    assert_eq!(filtered("b").await, [1]);
    assert_eq!(filtered("c").await, [2]);
    assert_eq!(filtered("missing").await, Vec::<u64>::new());
}

#[tokio::test]
async fn a_version_just_written_to_an_open_room_is_listed() {
    let fixture = Fixture::start().await;
    let room = fixture.state.rooms.get_or_open(ROOM).unwrap();
    let (out, _inbox) = outbox();
    room.attach(
        1,
        fixture.bob,
        "test@example.org".to_owned(),
        Role::Editor,
        out,
    )
    .unwrap();
    let update = fixture.record(create("a"));
    room.frame(
        1,
        Bytes::from(Message::Sync(SyncMessage::Update(update.to_vec())).encode_v1()),
    );

    let versions = fixture.history(fixture.alice).await;

    assert_eq!(ids(&versions), [0]);
    assert_eq!(versions[0]["author_email"], "bob@x.org");
    assert_eq!(versions[0]["events_added"], 1);
}

#[tokio::test]
async fn a_catalogue_filter_narrows_the_list() {
    let fixture = Fixture::start().await;
    fixture.write(fixture.alice, 0, |txn| {
        model::create_event(txn, &event("a")).unwrap();
        model::create_catalogue(txn, &catalogue("plain")).unwrap();
        assert!(model::add_catalogue_events(txn, "plain", &["a".to_owned()]));
    });
    fixture.write(fixture.bob, MINUTE, create("b"));
    fixture.write(fixture.alice, 2 * MINUTE, |txn| {
        model::create_catalogue(txn, &catalogue("smart")).unwrap();
        let rule = json!({"all": true});
        assert!(model::set_catalogue_rule(txn, "smart", Some(&rule)).unwrap());
    });
    let filtered = |catalogue: &'static str| {
        let fixture = &fixture;
        async move {
            let path = format!("/api/rooms/{ROOM}/history?catalogue={catalogue}");
            let (status, body) = fixture.get(&path, fixture.carol).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            ids(body["versions"].as_array().unwrap())
        }
    };

    assert_eq!(filtered("plain").await, [0]);
    assert_eq!(filtered("smart").await, [2, 1, 0]);
    assert_eq!(filtered("missing").await, Vec::<u64>::new());
}

#[tokio::test]
async fn the_diff_of_a_version_names_its_changes() {
    let fixture = two_versions().await;

    let (status, diff) = fixture
        .get(&format!("/api/rooms/{ROOM}/history/1"), fixture.carol)
        .await;

    assert_eq!(status, StatusCode::OK, "{diff}");
    let events = diff["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["uuid"], "a");
    assert_eq!(events[0]["change"], "changed");
    assert_eq!(events[0]["fields"], json!(["confidence"]));
    assert_eq!(events[0]["before"]["confidence"], Value::Null);
    assert_eq!(events[0]["after"]["confidence"], 0.5);
    assert_eq!(events[1]["uuid"], "b");
    assert_eq!(events[1]["change"], "added");

    let (_, first) = fixture
        .get(&format!("/api/rooms/{ROOM}/history/0"), fixture.carol)
        .await;
    assert_eq!(first["events"][0]["change"], "added");
    assert_eq!(first["events"][0]["before"], Value::Null);
}

#[tokio::test]
async fn the_diff_since_an_older_version_spans_every_version_after_it() {
    let fixture = two_versions().await;
    fixture.write(fixture.alice, 2 * MINUTE, create("c"));
    let changed = |diff: &Value| -> Vec<(String, String)> {
        diff["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| {
                let field = |key: &str| event[key].as_str().unwrap().to_owned();
                (field("uuid"), field("change"))
            })
            .collect()
    };

    let (status, since) = fixture
        .get(
            &format!("/api/rooms/{ROOM}/history/2?since=0"),
            fixture.carol,
        )
        .await;
    let (_, either_way) = fixture
        .get(
            &format!("/api/rooms/{ROOM}/history/0?since=2"),
            fixture.carol,
        )
        .await;
    let (_, alone) = fixture
        .get(&format!("/api/rooms/{ROOM}/history/2"), fixture.carol)
        .await;
    let (missing, _) = fixture
        .get(
            &format!("/api/rooms/{ROOM}/history/2?since=7"),
            fixture.carol,
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{since}");
    let both = |a: &str, b: &str| (a.to_owned(), b.to_owned());
    assert_eq!(
        changed(&since),
        [both("a", "changed"), both("b", "added"), both("c", "added")]
    );
    assert_eq!(since, either_way);
    assert_eq!(changed(&alone), [both("c", "added")]);
    assert_eq!(missing, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_state_of_a_version_replays_to_that_point() {
    let fixture = two_versions().await;

    let path = format!("/api/rooms/{ROOM}/history/0/state");
    let (status, content_type, bytes) = fixture.call(Method::GET, &path, fixture.carol, None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("application/octet-stream"));
    let past = Doc::new();
    past.transact_mut()
        .apply_update(Update::decode_v1(&bytes).unwrap())
        .unwrap();
    let txn = past.transact();
    assert_eq!(model::event(&txn, "a").unwrap().confidence, None);
    assert!(model::event(&txn, "b").is_none());
}

#[tokio::test]
async fn an_editor_names_a_version_and_the_list_shows_it() {
    let fixture = two_versions().await;

    let (status, body) = fixture.name(0, fixture.bob, "  paper v1 ").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(fixture.history(fixture.carol).await[1]["name"], "paper v1");

    let (status, _) = fixture.name(0, fixture.alice, "paper v2").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(fixture.history(fixture.carol).await[1]["name"], "paper v2");

    let path = format!("/api/rooms/{ROOM}/history/0/name");
    let (status, _, _) = fixture.call(Method::DELETE, &path, fixture.bob, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(fixture.history(fixture.carol).await[1]["name"], Value::Null);
}

#[tokio::test]
async fn a_name_and_the_id_follow_a_version_its_author_keeps_editing() {
    let fixture = Fixture::start().await;
    fixture.write(fixture.alice, 0, create("a"));
    assert_eq!(
        fixture.name(0, fixture.alice, "paper v1").await.0,
        StatusCode::NO_CONTENT
    );

    fixture.write(fixture.alice, MINUTE, create("b"));

    let versions = fixture.history(fixture.carol).await;
    assert_eq!(ids(&versions), [0]);
    assert_eq!(versions[0]["name"], "paper v1");
    let path = format!("/api/rooms/{ROOM}/history/0/state");
    let (status, _, bytes) = fixture.call(Method::GET, &path, fixture.carol, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(model::event(&doc_of(&bytes).transact(), "b").is_some());
}

#[tokio::test]
async fn a_name_stored_under_a_versions_last_record_still_shows() {
    let fixture = Fixture::start().await;
    fixture.write(fixture.alice, 0, create("a"));
    fixture.write(fixture.alice, MINUTE, create("b"));
    db::name_version(&fixture.state.pool, ROOM, 1..=1, "old row", fixture.alice)
        .await
        .unwrap();
    assert_eq!(fixture.history(fixture.carol).await[0]["name"], "old row");

    assert_eq!(
        fixture.name(0, fixture.alice, "renamed").await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(fixture.history(fixture.carol).await[0]["name"], "renamed");

    let path = format!("/api/rooms/{ROOM}/history/0/name");
    let (status, _, _) = fixture
        .call(Method::DELETE, &path, fixture.alice, None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(fixture.history(fixture.carol).await[0]["name"], Value::Null);
}

#[tokio::test]
async fn a_blank_or_long_name_is_refused() {
    let fixture = two_versions().await;

    for name in ["   ".to_owned(), "x".repeat(101)] {
        let (status, _) = fixture.name(0, fixture.bob, &name).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{name:?}");
    }
    assert_eq!(
        fixture.name(0, fixture.bob, &"x".repeat(100)).await.0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn a_viewer_cannot_name_a_version() {
    let fixture = two_versions().await;

    let (status, body) = fixture.name(0, fixture.carol, "mine").await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body["detail"],
        format!("Project {ROOM} requires the editor role")
    );
    assert_eq!(fixture.history(fixture.alice).await[1]["name"], Value::Null);
}

#[tokio::test]
async fn strangers_get_404() {
    let fixture = two_versions().await;
    let dave = db::create_user(&fixture.state.pool, "dave@x.org", "hunter2", false, true)
        .await
        .unwrap();

    for path in ["history", "history/0", "history/0/state"] {
        let (status, body) = fixture
            .get(&format!("/api/rooms/{ROOM}/{path}"), dave)
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body["detail"], format!("Project {ROOM} not found"));
    }
    assert_eq!(fixture.name(0, dave, "mine").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_unknown_version_is_404() {
    let fixture = Fixture::start().await;
    fixture.write(fixture.alice, 0, create("a"));
    fixture.write(fixture.alice, 1, create("b"));

    for path in ["history/1", "history/2", "history/1/state"] {
        let (status, body) = fixture
            .get(&format!("/api/rooms/{ROOM}/{path}"), fixture.carol)
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body["detail"], "No such version");
    }
    assert_eq!(
        fixture.name(1, fixture.bob, "x").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fixture.name(0, fixture.bob, "x").await.0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn a_static_catalogue_filter_keeps_its_members_event_edits() {
    let fixture = Fixture::start().await;
    fixture.write(fixture.alice, 0, |txn| {
        model::create_event(txn, &event("a")).unwrap();
        model::create_event(txn, &event("gone")).unwrap();
        model::create_catalogue(txn, &catalogue("plain")).unwrap();
        let members = ["a".to_owned(), "gone".to_owned()];
        assert!(model::add_catalogue_events(txn, "plain", &members));
    });
    fixture.write(fixture.bob, MINUTE, |txn| {
        model::set_event_field(txn, "a", &EventField::Confidence(Some(0.5))).unwrap();
    });
    fixture.write(fixture.alice, 2 * MINUTE, create("other"));
    fixture.write(fixture.bob, 3 * MINUTE, |txn| {
        assert!(model::delete_event(txn, "gone"));
    });

    let path = format!("/api/rooms/{ROOM}/history?catalogue=plain");
    let (status, body) = fixture.get(&path, fixture.carol).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(body["versions"].as_array().unwrap()), [3, 1, 0]);
}

#[tokio::test]
async fn a_catalogue_filter_counts_only_its_members_events() {
    let fixture = Fixture::start().await;
    fixture.write(fixture.alice, 0, |txn| {
        model::create_event(txn, &event("a")).unwrap();
        model::create_event(txn, &event("b")).unwrap();
        model::create_catalogue(txn, &catalogue("plain")).unwrap();
        assert!(model::add_catalogue_events(txn, "plain", &["a".to_owned()]));
    });
    fixture.write(fixture.bob, MINUTE, |txn| {
        for uuid in ["a", "b"] {
            model::set_event_field(txn, uuid, &EventField::Confidence(Some(0.5))).unwrap();
        }
        model::create_event(txn, &event("c")).unwrap();
    });
    let counts = |versions: &[Value]| -> Vec<(u64, u64, u64)> {
        versions
            .iter()
            .map(|version| {
                let count = |key: &str| version[key].as_u64().unwrap();
                (
                    count("events_added"),
                    count("events_changed"),
                    count("events_removed"),
                )
            })
            .collect()
    };

    let path = format!("/api/rooms/{ROOM}/history?catalogue=plain");
    let (status, body) = fixture.get(&path, fixture.carol).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        counts(body["versions"].as_array().unwrap()),
        [(0, 1, 0), (1, 0, 0)]
    );
    assert_eq!(
        counts(&fixture.history(fixture.carol).await),
        [(1, 2, 0), (2, 0, 0)]
    );
}

#[tokio::test]
async fn restore_is_broadcast_to_a_connected_client_and_stamped_with_the_restorer() {
    let fixture = two_versions().await;
    let room = fixture.state.rooms.get_or_open(ROOM).unwrap();
    let (out, mut inbox) = outbox();
    room.attach(
        1,
        fixture.carol,
        "test@example.org".to_owned(),
        Role::Viewer,
        out,
    )
    .unwrap();
    inbox.frames.recv().await.expect("the sync step 1");

    let (status, body) = fixture.restore(0, fixture.alice).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "restored": true }));
    let frame = inbox.frames.recv().await.expect("the restore");
    let Message::Sync(SyncMessage::Update(update)) = Message::decode_v1(&frame).unwrap() else {
        panic!("the restore is not an update frame");
    };
    fixture
        .doc
        .transact_mut()
        .apply_update(Update::decode_v1(&update).unwrap())
        .unwrap();
    let txn = fixture.doc.transact();
    assert_eq!(model::event(&txn, "a").unwrap().confidence, None);
    assert_eq!(
        model::event(&txn, "b").unwrap().status,
        model::Status::Deleted
    );
    let versions = fixture.history(fixture.carol).await;
    assert_eq!(ids(&versions), [2, 1, 0]);
    assert_eq!(versions[0]["author_email"], "alice@x.org");
}

#[tokio::test]
async fn a_viewer_cannot_restore() {
    let fixture = two_versions().await;

    let (status, body) = fixture.restore(0, fixture.carol).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body["detail"],
        format!("Project {ROOM} requires the editor role")
    );
    assert_eq!(ids(&fixture.history(fixture.alice).await), [1, 0]);
}

#[tokio::test]
async fn a_restore_is_itself_a_version_and_can_be_undone() {
    let fixture = two_versions().await;

    assert_eq!(fixture.restore(0, fixture.alice).await.0, StatusCode::OK);
    fixture.state.rooms.flush(ROOM).await.unwrap();
    assert!(
        !fixture.state.rooms.is_open(ROOM),
        "a room opened for a restore stays open"
    );
    let restored = fixture.latest().await;
    assert_eq!(
        model::event(&restored.transact(), "b").unwrap().status,
        model::Status::Deleted
    );

    let (status, body) = fixture.restore(1, fixture.bob).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "restored": true }));
    assert_eq!(ids(&fixture.history(fixture.alice).await), [3, 2, 1, 0]);
    let undone = fixture.latest().await;
    let txn = undone.transact();
    assert_eq!(model::event(&txn, "a").unwrap().confidence, Some(0.5));
    assert_eq!(
        model::event(&txn, "b").unwrap().status,
        model::Status::Confirmed
    );

    let (_, again) = fixture.restore(1, fixture.bob).await;
    assert_eq!(again, json!({ "restored": false }));
}

#[tokio::test]
async fn a_restore_stays_apart_from_the_restorers_own_edits() {
    let fixture = Fixture::start().await;
    let now = now_ms();
    fixture.write(fixture.alice, now, create("a"));
    fixture.write(fixture.alice, now, create("b"));
    fixture.write(fixture.bob, now, create("c"));
    fixture.write(fixture.alice, now, create("d"));
    let restored = ids(&fixture.history(fixture.alice).await)[1];

    assert_eq!(
        fixture.restore(restored, fixture.alice).await.0,
        StatusCode::OK
    );

    let versions = fixture.history(fixture.alice).await;
    assert_eq!(versions.len(), 4, "{versions:?}");
    assert_eq!(versions[0]["restored_from"], json!(restored));
    assert_eq!(versions[0]["restored_from_ms"], versions[2]["ended_ms"]);
    assert_eq!(versions[0]["author_email"], "alice@x.org");
    assert_eq!(versions[1]["restored_from"], Value::Null);
}

#[tokio::test]
async fn restoring_an_unknown_version_is_404() {
    let fixture = two_versions().await;

    let (status, body) = fixture.restore(7, fixture.bob).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["detail"], "No such version");
}

impl Fixture {
    async fn save(&self, user: Uuid, body: Value) -> (StatusCode, Value) {
        let path = format!("/api/rooms/{ROOM}/history/commit");
        let (status, _, bytes) = self.call(Method::POST, &path, user, Some(body)).await;
        (status, json_of(&bytes))
    }
}

#[tokio::test]
async fn an_editor_saves_a_version_with_a_name_and_a_note() {
    let fixture = Fixture::start().await;
    fixture.write(fixture.alice, 0, create("a"));
    fixture.write(fixture.alice, MINUTE, create("b"));

    let (status, body) = fixture
        .save(
            fixture.bob,
            json!({ "name": "checkpoint", "message": "  looked at a and b  " }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "version": 0 }));
    let versions = fixture.history(fixture.carol).await;
    assert_eq!(versions[0]["committed"], true);
    assert_eq!(versions[0]["name"], "checkpoint");
    assert_eq!(versions[0]["message"], "looked at a and b");
}

#[tokio::test]
async fn a_saved_version_ends_where_it_was_saved() {
    let fixture = Fixture::start().await;
    fixture.write(fixture.alice, 0, create("a"));
    assert_eq!(
        fixture.save(fixture.alice, json!({})).await.0,
        StatusCode::OK
    );
    fixture.write(fixture.alice, MINUTE, create("b"));

    let versions = fixture.history(fixture.alice).await;

    assert_eq!(ids(&versions), [1, 0]);
    assert_eq!(versions[0]["committed"], false);
    assert_eq!(versions[0]["message"], Value::Null);
    assert_eq!(versions[1]["committed"], true);
    assert_eq!(versions[1]["message"], Value::Null);
    assert_eq!(versions[1]["name"], Value::Null);
}

#[tokio::test]
async fn saving_with_nothing_new_is_409() {
    let fixture = Fixture::start().await;
    let nothing = |(status, body): (StatusCode, Value)| {
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(
            body["detail"],
            "Nothing changed since the last saved version."
        );
    };

    nothing(fixture.save(fixture.alice, json!({})).await);
    fixture.write(fixture.alice, 0, create("a"));
    assert_eq!(
        fixture.save(fixture.alice, json!({})).await.0,
        StatusCode::OK
    );
    nothing(
        fixture
            .save(fixture.bob, json!({ "message": "again" }))
            .await,
    );
}

#[tokio::test]
async fn saving_needs_the_editor_role() {
    let fixture = two_versions().await;
    let dave = db::create_user(&fixture.state.pool, "dave@x.org", "hunter2", false, true)
        .await
        .unwrap();

    assert_eq!(
        fixture.save(fixture.carol, json!({})).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(fixture.save(dave, json!({})).await.0, StatusCode::NOT_FOUND);
    let versions = fixture.history(fixture.alice).await;
    assert!(versions.iter().all(|version| version["committed"] == false));
}

#[tokio::test]
async fn a_long_note_or_a_blank_name_is_refused() {
    let fixture = two_versions().await;
    let long = "x".repeat(2001);

    for body in [json!({ "message": long }), json!({ "name": "  " })] {
        let (status, _) = fixture.save(fixture.bob, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    let fits = json!({ "message": "x".repeat(2000) });
    assert_eq!(fixture.save(fixture.bob, fits).await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_blank_note_is_no_note() {
    let fixture = two_versions().await;

    assert_eq!(
        fixture
            .save(fixture.bob, json!({ "message": " \n " }))
            .await
            .0,
        StatusCode::OK
    );

    assert_eq!(
        fixture.history(fixture.bob).await[0]["message"],
        Value::Null
    );
}

#[tokio::test]
async fn saving_an_open_room_covers_what_it_just_took() {
    let fixture = Fixture::start().await;
    fixture.write(fixture.alice, 0, create("a"));
    let room = fixture.state.rooms.get_or_open(ROOM).unwrap();
    let (out, _inbox) = outbox();
    room.attach(1, fixture.bob, "bob@x.org".to_owned(), Role::Editor, out)
        .unwrap();
    let update = fixture.record(create("b"));
    room.frame(
        1,
        Bytes::from(Message::Sync(SyncMessage::Update(update.to_vec())).encode_v1()),
    );

    let (status, body) = fixture.save(fixture.bob, json!({})).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "version": 1 }));
    let versions = fixture.history(fixture.alice).await;
    assert_eq!(ids(&versions), [1, 0]);
    assert_eq!(versions[0]["committed"], true);
    assert_eq!(versions[0]["last"], 1);
}

/// 2023-11-14 22:13:20 UTC.
const COPIED_AT: i64 = 1_700_000_000_000;

impl Fixture {
    async fn copy(&self, version: u64, user: Uuid, name: &str) -> (StatusCode, Value) {
        let path = format!("/api/rooms/{ROOM}/history/{version}/copy");
        let body = Some(json!({ "name": name }));
        let (status, _, bytes) = self.call(Method::POST, &path, user, body).await;
        (status, json_of(&bytes))
    }

    async fn history_of(&self, room: &str, user: Uuid) -> Vec<Value> {
        let (status, body) = self.get(&format!("/api/rooms/{room}/history"), user).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["versions"].as_array().unwrap().clone()
    }

    async fn state_of(&self, room: &str, version: u64, user: Uuid) -> Doc {
        let path = format!("/api/rooms/{room}/history/{version}/state");
        let (status, _, bytes) = self.call(Method::GET, &path, user, None).await;
        assert_eq!(status, StatusCode::OK);
        doc_of(&bytes)
    }
}

/// Every root the model keeps, as plain values.
fn contents(doc: &Doc) -> Vec<yrs::Any> {
    let txn = doc.transact();
    [
        model::CATALOGUES,
        model::EVENTS,
        model::ORIGINS,
        model::COMMENTS,
        model::REGISTRY,
        model::META,
    ]
    .into_iter()
    .map(|root| {
        txn.get_map(root).map_or(yrs::Any::Null, |map| {
            yrs::types::ToJson::to_json(&map, &txn)
        })
    })
    .collect()
}

/// Version 0: Alice adds `a`, a smart catalogue, and `gone` in the trash. Version 1: Bob
/// changes `a` and adds `b`.
async fn a_project_worth_copying() -> Fixture {
    let fixture = Fixture::start().await;
    fixture.write(fixture.alice, COPIED_AT, |txn| {
        model::create_event(txn, &event("a")).unwrap();
        model::create_event(txn, &event("gone")).unwrap();
        assert!(model::delete_event(txn, "gone"));
        model::create_catalogue(txn, &catalogue("smart")).unwrap();
        let rule = json!({"all": true});
        assert!(model::set_catalogue_rule(txn, "smart", Some(&rule)).unwrap());
    });
    fixture.write(fixture.bob, COPIED_AT + MINUTE, |txn| {
        model::set_event_field(txn, "a", &EventField::Confidence(Some(0.5))).unwrap();
        model::create_event(txn, &event("b")).unwrap();
    });
    fixture
}

#[tokio::test]
async fn a_viewer_copies_a_version_into_a_project_they_own() {
    let fixture = a_project_worth_copying().await;

    let (status, body) = fixture.copy(0, fixture.carol, "  Carol's try ").await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["name"], "Carol's try");
    let copy = body["room"].as_str().unwrap().to_owned();
    assert_ne!(copy, ROOM);
    let rooms = db::list_user_rooms(&fixture.state.pool, fixture.carol)
        .await
        .unwrap();
    let (room, role) = rooms.iter().find(|(room, _)| room.id == copy).unwrap();
    assert_eq!(room.name, "Carol's try");
    assert_eq!(*role, Role::Owner);
    assert!(
        db::role_in(&fixture.state.pool, &copy, fixture.alice)
            .await
            .unwrap()
            .is_none(),
        "the original's members are not in the copy"
    );
}

#[tokio::test]
async fn a_copy_holds_the_version_with_its_uuids_and_starts_with_one_named_version() {
    let fixture = a_project_worth_copying().await;
    let original = fixture.state_of(ROOM, 0, fixture.carol).await;

    let (_, body) = fixture.copy(0, fixture.carol, "try").await;

    let copy = body["room"].as_str().unwrap();
    let versions = fixture.history_of(copy, fixture.carol).await;
    assert_eq!(ids(&versions), [0]);
    assert_eq!(versions[0]["author_email"], "carol@x.org");
    assert_eq!(
        versions[0]["name"],
        "Copied from History, 2023-11-14 22:13 UTC"
    );
    let copied = fixture.state_of(copy, 0, fixture.carol).await;
    assert_eq!(contents(&copied), contents(&original));
    let txn = copied.transact();
    assert_eq!(model::event(&txn, "a").unwrap().confidence, None);
    assert!(model::event(&txn, "b").is_none());
    assert_eq!(
        model::event(&txn, "gone").unwrap().status,
        model::Status::Deleted
    );
    assert!(model::catalogue(&txn, "smart").unwrap().rule.is_some());
}

#[tokio::test]
async fn copying_leaves_the_original_and_its_history_alone() {
    let fixture = a_project_worth_copying().await;
    let before = fixture.history(fixture.alice).await;
    let latest = contents(&fixture.latest().await);

    assert_eq!(
        fixture.copy(0, fixture.bob, "try").await.0,
        StatusCode::CREATED
    );

    assert_eq!(fixture.history(fixture.alice).await, before);
    assert_eq!(contents(&fixture.latest().await), latest);
}

#[tokio::test]
async fn a_stranger_cannot_copy_and_an_unknown_version_is_404() {
    let fixture = a_project_worth_copying().await;
    let dave = db::create_user(&fixture.state.pool, "dave@x.org", "hunter2", false, true)
        .await
        .unwrap();

    let (status, body) = fixture.copy(0, dave, "mine").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["detail"], format!("Project {ROOM} not found"));
    let (status, body) = fixture.copy(7, fixture.carol, "mine").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["detail"], "No such version");

    for user in [dave, fixture.carol] {
        let rooms = db::list_user_rooms(&fixture.state.pool, user)
            .await
            .unwrap();
        assert!(rooms.iter().all(|(room, _)| room.id == ROOM), "{rooms:?}");
    }
}

#[tokio::test]
async fn a_copy_name_follows_the_project_name_rule() {
    let fixture = a_project_worth_copying().await;

    let (status, body) = fixture.copy(0, fixture.carol, "   ").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["detail"], "Enter a project name");
    let long = "x".repeat(201);
    assert_eq!(
        fixture.copy(0, fixture.carol, &long).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[tokio::test]
async fn a_copy_over_the_owners_quota_is_refused_and_leaves_no_project() {
    let fixture = a_project_worth_copying().await;
    db::create_room(
        &fixture.state.pool,
        fixture.carol,
        "carols-big",
        "Big",
        db::ProjectKind::Catalogues,
    )
    .await
    .unwrap();
    fs::write(
        cocat_core::store::room_file(&fixture.update_dir, "carols-big"),
        vec![0u8; 1024 * 1024],
    )
    .unwrap();
    db::set_user_quota(&fixture.state.pool, fixture.carol, Some(1))
        .await
        .unwrap();

    let (status, body) = fixture.copy(0, fixture.carol, "try").await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["detail"], "The project reached its storage limit");
    let rooms = db::list_user_rooms(&fixture.state.pool, fixture.carol)
        .await
        .unwrap();
    let mut ids: Vec<_> = rooms.into_iter().map(|(room, _)| room.id).collect();
    ids.sort();
    assert_eq!(ids, ["carols-big", ROOM]);
}

#[tokio::test]
async fn a_copy_whose_first_version_cannot_be_named_leaves_no_project() {
    let fixture = a_project_worth_copying().await;
    sqlx::query(
        "CREATE TRIGGER refuse_names BEFORE INSERT ON version_name \
         BEGIN SELECT RAISE(ABORT, 'refused'); END",
    )
    .execute(&fixture.state.pool)
    .await
    .unwrap();

    let (status, body) = fixture.copy(0, fixture.carol, "try").await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    let rooms = db::list_user_rooms(&fixture.state.pool, fixture.carol)
        .await
        .unwrap();
    let ids: Vec<_> = rooms.into_iter().map(|(room, _)| room.id).collect();
    assert_eq!(ids, [ROOM]);
}
