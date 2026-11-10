//! Submissions to the public catalogues: a new catalogue, or a change proposed on a copy of
//! a public one; curators accept or decline, authors withdraw.

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use cocat_core::model::{self, EventField, EventRecord, Hop, NewCatalogue, Status};
use cocat_core::role::Role;
use cocat_core::time::Timestamp;
use cocat_server::room::{Author, RoomManager, StorageLimits};
use cocat_server::{
    AppState, auth, db, lifecycle, links::Links, ratelimit::LoginLimit, submissions, ws,
};
use serde_json::{Value, json};
use std::fs;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;
use yrs::updates::decoder::Decode;
use yrs::{Doc, ReadTxn, StateVector, Transact, TransactionMut, Update};

/// A curator, an author with a project of their own, and a stranger to both.
struct World {
    state: AppState,
    curator: Uuid,
    author: Uuid,
    stranger: Uuid,
    project: String,
    _dir: TempDir,
}

impl World {
    async fn start() -> Self {
        Self::with_limits(StorageLimits::default()).await
    }

    async fn with_limits(limits: StorageLimits) -> Self {
        let dir = TempDir::new().unwrap();
        let update_dir = dir.path().join("rooms");
        fs::create_dir_all(&update_dir).unwrap();
        let pool = db::open(&dir.path().join("test.db")).await.unwrap();
        let state = AppState {
            pool,
            secret: "test-secret".to_owned(),
            rooms: RoomManager::new(update_dir, None).with_storage(limits),
            before_attach: None,
            login_limit: LoginLimit::default(),
            mailer: None,
            links: Links::default(),
            oidc: Vec::new(),
            web_dir: None,
        };
        let account = |email: &'static str| db::create_user(&state.pool, email, "pwd", false, true);
        let curator = account("curator@example.org").await.unwrap();
        db::set_curator(&state.pool, curator, true).await.unwrap();
        let author = account("author@example.org").await.unwrap();
        let stranger = account("stranger@example.org").await.unwrap();
        let project = lifecycle::create_room(&state, author, "Lab")
            .await
            .unwrap()
            .id;
        Self {
            state,
            curator,
            author,
            stranger,
            project,
            _dir: dir,
        }
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

    /// Writes `edit` into the room as `user` would from a client.
    async fn edit(
        &self,
        room: &str,
        user: Uuid,
        edit: impl FnOnce(&mut TransactionMut) + Send + 'static,
    ) {
        let doc = self.doc(room).await;
        let seen = doc.transact().state_vector();
        edit(&mut doc.transact_mut());
        let update = doc.transact().encode_state_as_update_v1(&seen);
        self.state
            .rooms
            .write_as(room, u64::MAX, Author::Client(user), update)
            .await
            .unwrap();
    }

    async fn doc(&self, room: &str) -> Doc {
        let state = self
            .state
            .rooms
            .read(room, |doc| {
                doc.transact()
                    .encode_state_as_update_v1(&StateVector::default())
            })
            .await
            .unwrap();
        let doc = Doc::new();
        doc.transact_mut()
            .apply_update(Update::decode_v1(&state).unwrap())
            .unwrap();
        doc
    }

    /// A public catalogue holding catalogue `pub` with events `p1` and `p2`.
    async fn public(&self) -> String {
        let (status, made) = self
            .call(
                self.curator,
                Method::POST,
                "/api/public",
                Some(json!({"name": "Shocks"})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        let id = made["id"].as_str().unwrap().to_owned();
        self.edit(&id, self.curator, |txn| {
            catalogue(txn, "pub", &["p1", "p2"])
        })
        .await;
        id
    }

    async fn submit(&self, body: Value) -> (StatusCode, Value) {
        self.call(
            self.author,
            Method::POST,
            "/api/public/submissions",
            Some(body),
        )
        .await
    }

    async fn detail(&self, caller: Uuid, id: i64) -> (StatusCode, Value) {
        let path = format!("/api/public/submissions/{id}");
        self.call(caller, Method::GET, &path, None).await
    }

    async fn review(&self, caller: Uuid, id: i64, body: Value) -> (StatusCode, Value) {
        let path = format!("/api/public/submissions/{id}/review");
        self.call(caller, Method::POST, &path, Some(body)).await
    }

    /// The author proposes a change on `public` into a new project; its id.
    async fn propose(&self, public: &str) -> String {
        let path = format!("/api/public/{public}/proposals");
        let (status, copied) = self
            .call(self.author, Method::POST, &path, Some(json!({})))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{copied}");
        assert_eq!(copied["catalogue"], "pub");
        copied["room"].as_str().unwrap().to_owned()
    }
}

fn at(day: i64) -> Timestamp {
    Timestamp::from_unix_nanos(day * 86_400_000_000_000).unwrap()
}

/// Events `events` (day 1, 2, …) and catalogue `uuid` listing them.
fn catalogue(txn: &mut TransactionMut, uuid: &str, events: &[&str]) {
    for (day, event) in events.iter().enumerate() {
        let day = i64::try_from(day).unwrap() + 1;
        model::create_event(txn, &EventRecord::new(*event, at(day), at(day))).unwrap();
    }
    let new = NewCatalogue {
        uuid: uuid.to_owned(),
        name: format!("{uuid} name"),
        ..NewCatalogue::default()
    };
    model::create_catalogue(txn, &new).unwrap();
    let events: Vec<String> = events.iter().map(|event| (*event).to_owned()).collect();
    assert!(model::add_catalogue_events(txn, uuid, &events));
}

fn changes(detail: &Value) -> Vec<(String, String)> {
    let mut changes: Vec<(String, String)> = detail["changes"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| {
            (
                change["uuid"].as_str().unwrap().to_owned(),
                change["change"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    changes.sort();
    changes
}

fn pair(uuid: &str, change: &str) -> (String, String) {
    (uuid.to_owned(), change.to_owned())
}

#[tokio::test]
async fn a_submission_holds_its_catalogue_as_it_was_and_nothing_else() {
    let world = World::start().await;
    world
        .edit(&world.project, world.author, |txn| {
            catalogue(txn, "mine", &["a", "b"]);
            catalogue(txn, "other", &["x"]);
        })
        .await;

    let (status, made) = world
        .submit(
            json!({"room": world.project, "catalogue": "mine", "folder": "Lab/Waves",
                       "description": "Two waves", "note": "please"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    world
        .edit(&world.project, world.author, |txn| {
            model::create_event(txn, &EventRecord::new("late", at(9), at(9))).unwrap();
            assert!(model::add_catalogue_events(
                txn,
                "mine",
                &["late".to_owned()]
            ));
        })
        .await;

    assert_eq!(made["kind"], "new");
    assert_eq!(made["title"], "mine name", "the title defaults to the name");
    assert_eq!(made["folder"], "Lab/Waves");
    assert_eq!(made["status"], "pending");
    assert_eq!(made["author"], "author@example.org");
    let id = made["id"].as_i64().unwrap();
    let (status, detail) = world.detail(world.curator, id).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(changes(&detail), [pair("a", "added"), pair("b", "added")]);
    assert_eq!(detail["stale"], false);
    assert_eq!(detail["note"], "please");
}

#[tokio::test]
async fn only_editors_submit_a_catalogue_that_exists() {
    let world = World::start().await;
    world
        .edit(&world.project, world.author, |txn| {
            catalogue(txn, "mine", &["a"])
        })
        .await;
    db::add_member(
        &world.state.pool,
        &world.project,
        world.stranger,
        Role::Viewer,
    )
    .await
    .unwrap();
    let body = json!({"room": world.project, "catalogue": "mine"});

    let (viewer, _) = world
        .call(
            world.stranger,
            Method::POST,
            "/api/public/submissions",
            Some(body),
        )
        .await;
    let (missing, said) = world
        .submit(json!({"room": world.project, "catalogue": "nope"}))
        .await;
    let (blank, _) = world
        .submit(json!({"room": world.project, "catalogue": "mine", "title": "  "}))
        .await;

    assert_eq!(viewer, StatusCode::FORBIDDEN);
    assert_eq!(missing, StatusCode::NOT_FOUND);
    assert_eq!(said["detail"], "No such catalogue in this project");
    assert_eq!(blank, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_catalogue_over_the_project_size_limit_is_refused() {
    let world = World::with_limits(StorageLimits {
        room: 200,
        owner: u64::MAX,
        ..StorageLimits::default()
    })
    .await;
    let many: Vec<String> = (0..20).map(|n| format!("event-{n}")).collect();
    world
        .edit(&world.project, world.author, move |txn| {
            let names: Vec<&str> = many.iter().map(String::as_str).collect();
            catalogue(txn, "big", &names);
        })
        .await;

    let (status, said) = world
        .submit(json!({"room": world.project, "catalogue": "big"}))
        .await;

    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let detail = said["detail"].as_str().unwrap();
    assert!(
        detail.starts_with("This catalogue is too big to submit"),
        "{detail}"
    );
}

#[tokio::test]
async fn proposing_a_change_copies_the_public_catalogue_and_remembers_where_from() {
    let world = World::start().await;
    let public = world.public().await;

    let copy = world.propose(&public).await;

    let doc = world.doc(&copy).await;
    let txn = doc.transact();
    assert_eq!(model::catalogue(&txn, "pub").unwrap().event_count, 2);
    assert!(model::event(&txn, "p1").is_some());
    assert_eq!(
        db::role_in(&world.state.pool, &copy, world.author)
            .await
            .unwrap(),
        Some(Role::Owner)
    );
    let path = format!("/api/rooms/{copy}/proposals");
    let (status, links) = world.call(world.author, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        links,
        json!([{"catalogue": "pub", "target": public, "name": "Shocks"}])
    );
    let again = format!("/api/public/{public}/proposals");
    let (twice, _) = world
        .call(
            world.author,
            Method::POST,
            &again,
            Some(json!({"room": copy})),
        )
        .await;
    assert_eq!(twice, StatusCode::CONFLICT, "the project holds it already");
    let (into_public, _) = world
        .call(
            world.curator,
            Method::POST,
            &again,
            Some(json!({"room": public})),
        )
        .await;
    assert_eq!(into_public, StatusCode::CONFLICT);
}

/// The author changes p1, drops p2 and adds n1 in their copy, then submits it.
async fn proposed_update(world: &World) -> (String, String, i64) {
    let public = world.public().await;
    let copy = world.propose(&public).await;
    world
        .edit(&copy, world.author, |txn| {
            model::set_event_field(txn, "p1", &EventField::Confidence(Some(0.5))).unwrap();
            model::create_event(txn, &EventRecord::new("n1", at(5), at(5))).unwrap();
            assert!(model::add_catalogue_events(txn, "pub", &["n1".to_owned()]));
            assert!(model::remove_catalogue_events(
                txn,
                "pub",
                &["p2".to_owned()]
            ));
        })
        .await;
    let (status, made) = world
        .call(
            world.author,
            Method::POST,
            "/api/public/submissions/updates",
            Some(json!({"room": copy, "catalogue": "pub", "note": "fixed p1"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    (public, copy, made["id"].as_i64().unwrap())
}

#[tokio::test]
async fn an_update_shows_what_changed_against_the_public_catalogue_now() {
    let world = World::start().await;
    let (public, _, id) = proposed_update(&world).await;

    let (_, fresh) = world.detail(world.curator, id).await;
    world
        .edit(&public, world.curator, |txn| {
            model::set_event_field(txn, "p2", &EventField::Confidence(Some(0.1))).unwrap();
        })
        .await;
    let (_, stale) = world.detail(world.curator, id).await;

    assert_eq!(fresh["kind"], "update");
    assert_eq!(fresh["target"], public);
    assert_eq!(fresh["title"], "Shocks");
    assert_eq!(
        changes(&fresh),
        [
            pair("n1", "added"),
            pair("p1", "changed"),
            pair("p2", "removed")
        ]
    );
    assert_eq!(fresh["stale"], false);
    assert_eq!(stale["stale"], true, "the public catalogue changed since");
    let p1 = &fresh["changes"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|change| change["uuid"] == "p1")
        .unwrap()["fields"];
    assert_eq!(p1, &json!(["confidence"]));
    let p2 = fresh["changes"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|change| change["uuid"] == "p2")
        .unwrap();
    assert_eq!(
        p2["after"]["status"], "deleted",
        "accepting moves what the submission left out to the trash"
    );
}

#[tokio::test]
async fn accepting_an_update_makes_the_public_catalogue_the_submission() {
    let world = World::start().await;
    let (public, _, id) = proposed_update(&world).await;

    let (status, decided) = world
        .review(world.curator, id, json!({"accept": true, "note": "thanks"}))
        .await;

    assert_eq!(status, StatusCode::OK, "{decided}");
    assert_eq!(decided["status"], "accepted");
    assert_eq!(decided["reviewer"], "curator@example.org");
    assert_eq!(decided["review_note"], "thanks");
    let doc = world.doc(&public).await;
    let txn = doc.transact();
    assert_eq!(model::event(&txn, "p1").unwrap().confidence, Some(0.5));
    assert_eq!(model::event(&txn, "p2").unwrap().status, Status::Deleted);
    assert!(model::event(&txn, "n1").is_some());
    let path = format!("/api/rooms/{public}/history");
    let (_, history) = world.call(world.curator, Method::GET, &path, None).await;
    let newest = &history["versions"][0];
    assert_eq!(newest["name"], format!("Submission {id}: Shocks"));
    assert_eq!(newest["committed"], true);
    assert_eq!(newest["author_email"], "author@example.org");
    assert_eq!(newest["events_added"], 1);
    let (_, after) = world.detail(world.author, id).await;
    assert_eq!(changes(&after), [], "nothing left to change");
}

#[tokio::test]
async fn accepting_a_new_one_makes_a_public_catalogue_named_after_it() {
    let world = World::start().await;
    world
        .edit(&world.project, world.author, |txn| {
            catalogue(txn, "mine", &["a", "b"])
        })
        .await;
    let mut ids = Vec::new();
    for _ in 0..2 {
        let (_, made) = world
            .submit(
                json!({"room": world.project, "catalogue": "mine", "title": "Ondes de choc é",
                           "folder": "Lab", "description": "Waves"}),
            )
            .await;
        ids.push(made["id"].as_i64().unwrap());
    }

    for id in &ids {
        let (status, decided) = world
            .review(world.curator, *id, json!({"accept": true}))
            .await;
        assert_eq!(status, StatusCode::OK, "{decided}");
    }

    let (_, index) = world
        .call(world.stranger, Method::GET, "/api/public", None)
        .await;
    let listed: Vec<(&str, &str, &str)> = index
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["id"].as_str().unwrap(),
                entry["name"].as_str().unwrap(),
                entry["folder"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            ("public.ondes-de-choc-e", "Ondes de choc é", "Lab"),
            ("public.ondes-de-choc-e-2", "Ondes de choc é", "Lab"),
        ]
    );
    let (_, first) = world.detail(world.author, ids[0]).await;
    assert_eq!(first["target"], "public.ondes-de-choc-e");
    let doc = world.doc("public.ondes-de-choc-e").await;
    assert_eq!(
        model::catalogue(&doc.transact(), "mine")
            .unwrap()
            .event_count,
        2
    );
    let path = "/api/rooms/public.ondes-de-choc-e/history";
    let (_, history) = world.call(world.curator, Method::GET, path, None).await;
    assert_eq!(
        history["versions"][0]["name"],
        format!("Submission {}: Ondes de choc é", ids[0])
    );
    assert_eq!(history["versions"][0]["author_email"], "author@example.org");
}

#[tokio::test]
async fn a_declined_submission_tells_its_author_why_and_is_decided_once() {
    let world = World::start().await;
    world
        .edit(&world.project, world.author, |txn| {
            catalogue(txn, "mine", &["a"])
        })
        .await;
    let (_, made) = world
        .submit(json!({"room": world.project, "catalogue": "mine"}))
        .await;
    let id = made["id"].as_i64().unwrap();

    let (by_author, _) = world
        .review(world.author, id, json!({"accept": true}))
        .await;
    let (status, _) = world
        .review(
            world.curator,
            id,
            json!({"accept": false, "note": "needs sources"}),
        )
        .await;
    let (again, _) = world
        .review(world.curator, id, json!({"accept": true}))
        .await;

    assert_eq!(by_author, StatusCode::FORBIDDEN);
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again, StatusCode::CONFLICT);
    let (_, mine) = world
        .call(
            world.author,
            Method::GET,
            "/api/public/submissions?mine=true",
            None,
        )
        .await;
    assert_eq!(mine[0]["status"], "declined");
    assert_eq!(mine[0]["review_note"], "needs sources");
    assert_eq!(
        mine[0]["reviewer"], "a curator",
        "an author never sees a curator's email"
    );
    let detail = format!("/api/public/submissions/{id}");
    let (_, seen) = world.call(world.author, Method::GET, &detail, None).await;
    assert_eq!(seen["reviewer"], "a curator");
    let (_, curated) = world.call(world.curator, Method::GET, &detail, None).await;
    assert_eq!(curated["reviewer"], "curator@example.org");
    assert!(mine[0]["decided"].is_string());
    let (_, index) = world
        .call(world.author, Method::GET, "/api/public", None)
        .await;
    assert_eq!(index, json!([]));
}

#[tokio::test]
async fn an_author_withdraws_a_pending_submission_and_only_they_may() {
    let world = World::start().await;
    world
        .edit(&world.project, world.author, |txn| {
            catalogue(txn, "mine", &["a"])
        })
        .await;
    let (_, made) = world
        .submit(json!({"room": world.project, "catalogue": "mine"}))
        .await;
    let id = made["id"].as_i64().unwrap();
    let path = format!("/api/public/submissions/{id}/withdraw");

    let (by_curator, _) = world.call(world.curator, Method::POST, &path, None).await;
    let (by_stranger, _) = world.call(world.stranger, Method::POST, &path, None).await;
    let (status, withdrawn) = world.call(world.author, Method::POST, &path, None).await;
    let (twice, _) = world.call(world.author, Method::POST, &path, None).await;
    let (review, _) = world
        .review(world.curator, id, json!({"accept": true}))
        .await;

    assert_eq!(by_curator, StatusCode::FORBIDDEN);
    assert_eq!(by_stranger, StatusCode::NOT_FOUND);
    assert_eq!(status, StatusCode::OK);
    assert_eq!(withdrawn["status"], "withdrawn");
    assert_eq!(twice, StatusCode::CONFLICT);
    assert_eq!(review, StatusCode::CONFLICT);
}

#[tokio::test]
async fn only_the_author_and_curators_see_a_submission() {
    let world = World::start().await;
    world
        .edit(&world.project, world.author, |txn| {
            catalogue(txn, "mine", &["a"])
        })
        .await;
    let (_, made) = world
        .submit(json!({"room": world.project, "catalogue": "mine"}))
        .await;
    let id = made["id"].as_i64().unwrap();

    let (stranger, _) = world.detail(world.stranger, id).await;
    let (author, _) = world.detail(world.author, id).await;
    let (unknown, _) = world.detail(world.curator, id + 100).await;
    let list = |caller| world.call(caller, Method::GET, "/api/public/submissions", None);
    let (_, theirs) = list(world.stranger).await;
    let (_, all) = list(world.curator).await;
    let (_, curators_own) = world
        .call(
            world.curator,
            Method::GET,
            "/api/public/submissions?mine=true",
            None,
        )
        .await;

    assert_eq!(stranger, StatusCode::NOT_FOUND);
    assert_eq!(author, StatusCode::OK);
    assert_eq!(unknown, StatusCode::NOT_FOUND);
    assert_eq!(theirs, json!([]), "anyone else lists only their own");
    assert_eq!(all.as_array().unwrap().len(), 1);
    assert_eq!(curators_own, json!([]));
    let (_, curator) = world
        .call(world.curator, Method::GET, "/api/me", None)
        .await;
    let (_, author) = world.call(world.author, Method::GET, "/api/me", None).await;
    assert_eq!(
        curator["is_curator"], true,
        "the app shows curators the queue"
    );
    assert_eq!(author["is_curator"], false);
}

#[tokio::test]
async fn after_an_accepted_update_the_same_copy_submits_again_without_being_stale() {
    let world = World::start().await;
    let (_, copy, id) = proposed_update(&world).await;
    let (status, decided) = world
        .review(world.curator, id, json!({"accept": true}))
        .await;
    assert_eq!(status, StatusCode::OK, "{decided}");
    world
        .edit(&copy, world.author, |txn| {
            model::set_event_field(txn, "p1", &EventField::Confidence(Some(0.9))).unwrap();
        })
        .await;

    let (status, again) = world
        .call(
            world.author,
            Method::POST,
            "/api/public/submissions/updates",
            Some(json!({"room": copy, "catalogue": "pub"})),
        )
        .await;

    assert_eq!(status, StatusCode::CREATED, "{again}");
    let (_, detail) = world
        .detail(world.curator, again["id"].as_i64().unwrap())
        .await;
    assert_eq!(detail["stale"], false, "the copy holds what was accepted");
    assert_eq!(changes(&detail), [pair("p1", "changed")]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_curators_accepting_the_same_title_at_once_get_two_public_catalogues() {
    let world = std::sync::Arc::new(World::start().await);
    world
        .edit(&world.project, world.author, |txn| {
            catalogue(txn, "mine", &["a", "b"])
        })
        .await;
    let second = db::create_user(&world.state.pool, "second@example.org", "pwd", false, true)
        .await
        .unwrap();
    db::set_curator(&world.state.pool, second, true)
        .await
        .unwrap();
    for round in 0..20 {
        let title = format!("Twins {round}");
        let mut ids = Vec::new();
        for _ in 0..2 {
            let body = json!({"room": world.project, "catalogue": "mine", "title": title});
            let (status, made) = world.submit(body).await;
            assert_eq!(status, StatusCode::CREATED, "{made}");
            ids.push(made["id"].as_i64().unwrap());
        }

        let accepting = [(world.curator, ids[0]), (second, ids[1])].map(|(curator, id)| {
            let world = world.clone();
            tokio::spawn(async move { world.review(curator, id, json!({"accept": true})).await })
        });
        let mut targets = Vec::new();
        for task in accepting {
            let (status, decided) = task.await.unwrap();
            assert_eq!(status, StatusCode::OK, "round {round}: {decided}");
            targets.push(decided["target"].as_str().unwrap().to_owned());
        }

        targets.sort();
        let key = format!("public.twins-{round}");
        assert_eq!(targets, [key.clone(), format!("{key}-2")]);
    }
}

#[tokio::test]
async fn an_author_waits_on_at_most_twenty_submissions() {
    let world = World::start().await;
    world
        .edit(&world.project, world.author, |txn| {
            catalogue(txn, "mine", &["a"])
        })
        .await;
    let body = json!({"room": world.project, "catalogue": "mine"});
    let mut first = None;
    for _ in 0..20 {
        let (status, made) = world.submit(body.clone()).await;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        first.get_or_insert(made["id"].as_i64().unwrap());
    }

    let (status, said) = world.submit(body.clone()).await;
    let withdraw = format!("/api/public/submissions/{}/withdraw", first.unwrap());
    world
        .call(world.author, Method::POST, &withdraw, None)
        .await;
    let (again, _) = world.submit(body).await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        said["detail"],
        "You have 20 submissions waiting for a curator: withdraw one, or wait for a decision, \
         before submitting another"
    );
    assert_eq!(
        again,
        StatusCode::CREATED,
        "a withdrawn one no longer waits"
    );
}

#[tokio::test]
async fn submissions_count_against_the_authors_storage() {
    let world = World::with_limits(StorageLimits {
        room: 1 << 30,
        owner: 1 << 20,
        ..StorageLimits::default()
    })
    .await;
    world
        .edit(&world.project, world.author, |txn| {
            catalogue(txn, "mine", &["a", "b"])
        })
        .await;
    let rooms = &world.state.rooms;
    let before = rooms
        .allowance(&world.state.pool, &world.project)
        .await
        .unwrap();

    let (_, made) = world
        .submit(json!({"room": world.project, "catalogue": "mine"}))
        .await;
    let id = made["id"].as_i64().unwrap();
    let withdraw = format!("/api/public/submissions/{id}/withdraw");
    world
        .call(world.author, Method::POST, &withdraw, None)
        .await;

    let stored = submissions::snapshot(&world.state.pool, id).await.unwrap();
    let after = rooms
        .allowance(&world.state.pool, &world.project)
        .await
        .unwrap();
    assert_eq!(
        before - after,
        u64::try_from(stored.len()).unwrap(),
        "a withdrawn submission still takes its author's space"
    );
}

#[tokio::test]
async fn a_submission_over_the_authors_storage_is_refused() {
    let world = World::with_limits(StorageLimits {
        room: 1 << 30,
        owner: 10,
        ..StorageLimits::default()
    })
    .await;
    world
        .edit(&world.project, world.author, |txn| {
            catalogue(txn, "mine", &["a"])
        })
        .await;

    let (status, said) = world
        .submit(json!({"room": world.project, "catalogue": "mine"}))
        .await;

    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let detail = said["detail"].as_str().unwrap();
    assert!(detail.starts_with("Your storage is full"), "{detail}");
}

#[tokio::test]
async fn proposing_a_change_to_a_catalogue_with_a_long_name_names_the_copy_within_the_limit() {
    let world = World::start().await;
    let long = "é".repeat(250);
    let room = db::PublicRoom {
        id: "public.long".to_owned(),
        name: long.clone(),
        folder: None,
        source: None,
        description: None,
        public_count: None,
        counted_file: None,
    };
    assert!(
        db::create_public_room(&world.state.pool, &room, world.curator)
            .await
            .unwrap()
    );
    world
        .edit(&room.id, world.curator, |txn| {
            catalogue(txn, "pub", &["p1"])
        })
        .await;

    let (status, copied) = world
        .call(
            world.author,
            Method::POST,
            "/api/public/public.long/proposals",
            Some(json!({})),
        )
        .await;

    assert_eq!(status, StatusCode::CREATED, "{copied}");
    let name = copied["name"].as_str().unwrap();
    assert_eq!(name.chars().count(), 200, "{name}");
    assert!(name.ends_with("é (proposed change)"), "{name}");
}

impl World {
    /// `caller` copies `public` into `room`, or a new project for `None`.
    async fn copy(&self, caller: Uuid, public: &str, room: Option<&str>) -> (StatusCode, Value) {
        let path = format!("/api/public/{public}/copies");
        self.call(caller, Method::POST, &path, Some(json!({"room": room})))
            .await
    }

    async fn links(&self, room: &str) -> Value {
        let path = format!("/api/rooms/{room}/proposals");
        let (status, links) = self.call(self.author, Method::GET, &path, None).await;
        assert_eq!(status, StatusCode::OK, "{links}");
        links
    }
}

#[tokio::test]
async fn a_copy_into_a_new_project_is_named_after_the_public_catalogue_and_links_nowhere() {
    let world = World::start().await;
    let public = world.public().await;

    let (status, copied) = world.copy(world.author, &public, None).await;

    assert_eq!(status, StatusCode::CREATED, "{copied}");
    assert_eq!(copied["name"], "Shocks");
    assert_eq!(copied["catalogue"], "pub");
    let room = copied["room"].as_str().unwrap();
    assert_eq!(
        db::role_in(&world.state.pool, room, world.author)
            .await
            .unwrap(),
        Some(Role::Owner)
    );
    let doc = world.doc(room).await;
    let found = model::catalogue(&doc.transact(), "pub").unwrap();
    assert_eq!(found.name, "pub name");
    assert_eq!(found.event_count, 2);
    assert_eq!(world.links(room).await, json!([]), "no Submit update on it");
}

#[tokio::test]
async fn a_copy_into_a_project_of_ones_own_lands_there_once() {
    let world = World::start().await;
    let public = world.public().await;

    let (status, copied) = world
        .copy(world.author, &public, Some(&world.project))
        .await;
    let (twice, said) = world
        .copy(world.author, &public, Some(&world.project))
        .await;

    assert_eq!(status, StatusCode::CREATED, "{copied}");
    assert_eq!(
        copied,
        json!({"room": world.project, "name": "Lab", "catalogue": "pub"})
    );
    let doc = world.doc(&world.project).await;
    assert_eq!(
        model::catalogue(&doc.transact(), "pub")
            .unwrap()
            .event_count,
        2
    );
    assert_eq!(world.links(&world.project).await, json!([]));
    assert_eq!(twice, StatusCode::CONFLICT);
    assert_eq!(
        said["detail"],
        "This project already holds a copy of this catalogue"
    );
}

#[tokio::test]
async fn copying_needs_a_project_one_can_write_to() {
    let world = World::start().await;
    let public = world.public().await;
    db::add_member(
        &world.state.pool,
        &world.project,
        world.stranger,
        Role::Viewer,
    )
    .await
    .unwrap();
    let theirs = lifecycle::create_room(&world.state, world.curator, "Theirs")
        .await
        .unwrap()
        .id;

    let (viewer, _) = world
        .copy(world.stranger, &public, Some(&world.project))
        .await;
    let (stranger, _) = world.copy(world.author, &public, Some(&theirs)).await;
    let (into_public, _) = world.copy(world.curator, &public, Some(&public)).await;
    let (unknown, said) = world.copy(world.author, "public.nope", None).await;
    let (anyone, _) = world.copy(world.stranger, &public, None).await;

    assert_eq!(viewer, StatusCode::FORBIDDEN);
    assert_eq!(stranger, StatusCode::NOT_FOUND);
    assert_eq!(into_public, StatusCode::CONFLICT);
    assert_eq!(unknown, StatusCode::NOT_FOUND);
    assert_eq!(said["detail"], "No such public catalogue");
    assert_eq!(anyone, StatusCode::CREATED, "every account may copy one");
}

#[tokio::test]
async fn a_copy_counts_against_the_destination_owners_storage() {
    let world = World::with_limits(StorageLimits {
        room: 1 << 30,
        owner: 10,
        ..StorageLimits::default()
    })
    .await;
    let public = world.public().await;
    let before = db::owned_rooms(&world.state.pool, world.author)
        .await
        .unwrap()
        .len();

    let (into_own, said) = world
        .copy(world.author, &public, Some(&world.project))
        .await;
    let (into_new, _) = world.copy(world.author, &public, None).await;

    assert_eq!(into_own, StatusCode::CONFLICT);
    assert_eq!(said["detail"], "The project reached its storage limit");
    assert_eq!(into_new, StatusCode::CONFLICT);
    let doc = world.doc(&world.project).await;
    assert!(model::catalogue(&doc.transact(), "pub").is_none());
    let after = db::owned_rooms(&world.state.pool, world.author)
        .await
        .unwrap()
        .len();
    assert_eq!(after, before, "the failed new project is removed");
}

#[tokio::test]
async fn a_copy_of_a_public_catalogue_with_a_long_name_names_its_project_within_the_limit() {
    let world = World::start().await;
    let room = db::PublicRoom {
        id: "public.long".to_owned(),
        name: "é".repeat(250),
        folder: None,
        source: None,
        description: None,
        public_count: None,
        counted_file: None,
    };
    assert!(
        db::create_public_room(&world.state.pool, &room, world.curator)
            .await
            .unwrap()
    );
    world
        .edit(&room.id, world.curator, |txn| {
            catalogue(txn, "pub", &["p1"])
        })
        .await;

    let (status, copied) = world.copy(world.author, "public.long", None).await;

    assert_eq!(status, StatusCode::CREATED, "{copied}");
    assert_eq!(copied["name"], "é".repeat(200));
}

#[tokio::test]
async fn a_public_catalogue_copies_into_a_project_with_files() {
    let world = World::start().await;
    let public = world.public().await;
    let with_files = lifecycle::create_project(
        &world.state,
        world.author,
        "Notebooks",
        db::ProjectKind::Workspace,
    )
    .await
    .unwrap()
    .id;

    let (status, copied) = world.copy(world.author, &public, Some(&with_files)).await;

    assert_eq!(status, StatusCode::CREATED, "{copied}");
    assert_eq!(copied["room"], with_files.as_str());
}

impl World {
    async fn chain(&self, room: &str, catalogue: &str) -> Vec<Hop> {
        let doc = self.doc(room).await;
        model::catalogue(&doc.transact(), catalogue)
            .unwrap()
            .copied_from
    }

    /// The author submits their copy of `catalogue` in `room` as a new public catalogue and
    /// the curator accepts it; its id.
    async fn publish(&self, room: &str, catalogue: &str) -> String {
        let (_, made) = self
            .submit(json!({"room": room, "catalogue": catalogue, "title": "Again"}))
            .await;
        let id = made["id"].as_i64().unwrap();
        let (status, decided) = self.review(self.curator, id, json!({"accept": true})).await;
        assert_eq!(status, StatusCode::OK, "{decided}");
        let (_, detail) = self.detail(self.author, id).await;
        detail["target"].as_str().unwrap().to_owned()
    }
}

fn hop_from(project: &str, project_name: &str, catalogue_name: &str, by: &str) -> impl Fn(&Hop) {
    let expected = (project.to_owned(), project_name.to_owned());
    let (catalogue_name, by) = (catalogue_name.to_owned(), by.to_owned());
    move |hop| {
        assert_eq!((hop.project.clone(), hop.project_name.clone()), expected);
        assert_eq!(hop.catalogue_name, catalogue_name);
        assert_eq!(hop.by.as_deref(), Some(by.as_str()));
        assert!(hop.at > 1_700_000_000_000, "{}", hop.at);
    }
}

#[tokio::test]
async fn a_copied_public_catalogue_knows_where_it_came_from() {
    let world = World::start().await;
    let public = world.public().await;
    let (_, new) = world.copy(world.author, &public, None).await;
    let (_, into) = world
        .copy(world.author, &public, Some(&world.project))
        .await;
    let proposed = world.propose(&public).await;

    for room in [new["room"].as_str().unwrap(), &world.project, &proposed] {
        let chain = world.chain(room, "pub").await;

        assert_eq!(chain.len(), 1, "{room}");
        hop_from(&public, "Shocks", "pub name", "author@example.org")(&chain[0]);
        assert_eq!(chain[0].catalogue, "pub");
        assert_eq!(
            chain[0].version,
            Some(0),
            "the public catalogue's one version"
        );
    }
    assert_eq!(into["room"], world.project.as_str());
}

#[tokio::test]
async fn a_copy_of_a_copy_lists_both_hops_oldest_first() {
    let world = World::start().await;
    let first = world.public().await;
    let (_, copied) = world.copy(world.author, &first, Some(&world.project)).await;
    let second = world.publish(&world.project, "pub").await;

    let (_, again) = world.copy(world.stranger, &second, None).await;

    let projects =
        |chain: Vec<Hop>| -> Vec<String> { chain.into_iter().map(|hop| hop.project).collect() };
    assert_eq!(copied["room"], world.project.as_str());
    assert_eq!(
        projects(world.chain(&second, "pub").await),
        [first.clone(), world.project.clone()]
    );
    let in_b = world.chain(again["room"].as_str().unwrap(), "pub").await;
    assert_eq!(
        projects(in_b.clone()),
        [first, world.project.clone(), second]
    );
    hop_from(&world.project, "Lab", "pub name", "author@example.org")(&in_b[1]);
}

#[tokio::test]
async fn accepting_an_update_keeps_the_public_chain() {
    let world = World::start().await;
    let (public, _, id) = proposed_update(&world).await;
    let before = world.chain(&public, "pub").await;

    world
        .review(world.curator, id, json!({"accept": true}))
        .await;

    assert_eq!(world.chain(&public, "pub").await, before);
    assert!(before.is_empty(), "this public catalogue is an original");
}

#[tokio::test]
async fn accepting_an_update_does_not_replace_the_public_chain_with_the_copys() {
    let world = World::start().await;
    let public = world.public().await;
    let origin = Hop {
        project: "elsewhere".to_owned(),
        project_name: "Elsewhere".to_owned(),
        catalogue: "pub".to_owned(),
        catalogue_name: "pub name".to_owned(),
        version: None,
        at: 1_700_000_000_000,
        by: None,
    };
    let kept = origin.clone();
    world
        .edit(&public, world.curator, move |txn| {
            model::set_copied_from(txn, "pub", &[kept]);
        })
        .await;
    let copy = world.propose(&public).await;
    assert_eq!(world.chain(&copy, "pub").await.len(), 2);
    let (_, made) = world
        .call(
            world.author,
            Method::POST,
            "/api/public/submissions/updates",
            Some(json!({"room": copy, "catalogue": "pub"})),
        )
        .await;

    world
        .review(
            world.curator,
            made["id"].as_i64().unwrap(),
            json!({"accept": true}),
        )
        .await;

    assert_eq!(world.chain(&public, "pub").await, [origin]);
}

fn hop_by(project: &str, by: Option<&str>) -> Hop {
    Hop {
        project: project.to_owned(),
        project_name: project.to_owned(),
        catalogue: "pub".to_owned(),
        catalogue_name: "pub name".to_owned(),
        version: None,
        at: 1_700_000_000_000,
        by: by.map(str::to_owned),
    }
}

impl World {
    async fn submit_copy_of_copy(&self) -> i64 {
        let public = self.public().await;
        self.copy(self.author, &public, Some(&self.project)).await;
        let carols = vec![hop_by("p1", Some("carol@example.org"))];
        self.edit(&self.project, self.author, move |txn| {
            model::set_copied_from(txn, "pub", &carols);
        })
        .await;
        let (_, made) = self
            .submit(json!({"room": self.project, "catalogue": "pub", "title": "Again"}))
            .await;
        made["id"].as_i64().unwrap()
    }

    async fn accept(&self, id: i64) -> String {
        let (status, decided) = self.review(self.curator, id, json!({"accept": true})).await;
        assert_eq!(status, StatusCode::OK, "{decided}");
        let (_, detail) = self.detail(self.author, id).await;
        detail["target"].as_str().unwrap().to_owned()
    }
}

#[tokio::test]
async fn a_public_chain_names_only_the_submitting_author() {
    let world = World::start().await;
    let id = world.submit_copy_of_copy().await;

    let public = world.accept(id).await;

    let chain = world.chain(&public, "pub").await;
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0].project, "p1");
    assert_eq!(chain[0].by, None, "a copier's email stays private");
    assert_eq!(chain[1].by.as_deref(), Some("author@example.org"));
}

#[tokio::test]
async fn the_author_hop_is_taken_when_submitting_not_when_accepting() {
    let world = World::start().await;
    let id = world.submit_copy_of_copy().await;
    lifecycle::rename_room(&world.state, &world.project, world.author, "Renamed")
        .await
        .unwrap();

    let public = world.accept(id).await;

    let chain = world.chain(&public, "pub").await;
    assert_eq!(chain.last().unwrap().project_name, "Lab");
}

#[tokio::test]
async fn the_author_hop_survives_deleting_the_authors_project_before_acceptance() {
    let world = World::start().await;
    let id = world.submit_copy_of_copy().await;
    lifecycle::delete_room(&world.state, &world.project, world.author)
        .await
        .unwrap();

    let public = world.accept(id).await;

    let chain = world.chain(&public, "pub").await;
    let last = chain.last().unwrap();
    assert_eq!(
        (last.project.as_str(), last.project_name.as_str()),
        (world.project.as_str(), "Lab")
    );
}
