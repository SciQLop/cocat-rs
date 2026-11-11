use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use bytes::Bytes;
use cocat_core::model::{self, EventRecord, NewCatalogue};
use cocat_core::role::Role;
use cocat_core::store::FileRoomStore;
use cocat_core::time::Timestamp;
use cocat_server::room::{Author, RoomManager};
use cocat_server::{AppState, auth, db, links::Links, ratelimit::LoginLimit, ws};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;
use yrs::updates::decoder::Decode;
use yrs::{Doc, ReadTxn, StateVector, Transact, Update};

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
}

const CATALOGUE: &str = "aaaaaaaa-0000-4000-8000-000000000001";

fn budget() -> Value {
    json!({
        "cocat:attribute": "mode",
        "cocat:cap": {"qudt:numericValue": 10, "qudt:hasUnit": {"skos:notation": "Gibyte"}},
        "cocat:rates": [{
            "cocat:value": "burst",
            "cocat:rate": {"qudt:numericValue": 5, "qudt:hasUnit": {"skos:notation": "Mibyte/min"}},
        }],
    })
}

impl Fixture {
    async fn editor(&self, room: &str, email: &str, role: Role) -> (Uuid, String) {
        let (user, token) = self.user_token(email).await;
        db::add_member(&self.state.pool, room, user, role)
            .await
            .unwrap();
        (user, token)
    }

    fn seed_catalogue(&self, room: &str) {
        let doc = Doc::new();
        {
            let mut txn = doc.transact_mut();
            let catalogue = NewCatalogue {
                uuid: CATALOGUE.to_owned(),
                name: "final".to_owned(),
                ..NewCatalogue::default()
            };
            model::create_catalogue(&mut txn, &catalogue).unwrap();
        }
        let update = doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        let (mut store, _) = FileRoomStore::open(&self.rooms_dir, room).unwrap();
        store.append(Bytes::from(update)).unwrap();
        store.flush().unwrap();
    }

    async fn round(&self, token: &str, room: &str, body: Value) -> (StatusCode, Value) {
        self.send(
            Method::POST,
            &format!("/api/rooms/{room}/rounds"),
            token,
            Some(body),
        )
        .await
    }

    async fn move_to(
        &self,
        token: &str,
        room: &str,
        round: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let path = format!("/api/rooms/{room}/rounds/{round}/state");
        self.send(Method::POST, &path, token, Some(body)).await
    }
}

fn new_round() -> Value {
    json!({"name": "Round 1", "visibility": "open"})
}

#[tokio::test]
async fn an_editor_creates_a_round_and_a_viewer_cannot() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let (_, editor) = fixture
        .editor(&room, "editor@example.org", Role::Editor)
        .await;
    let (_, viewer) = fixture
        .editor(&room, "viewer@example.org", Role::Viewer)
        .await;

    let (status, body) = fixture.round(&editor, &room, new_round()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["state"], "proposing");
    assert_eq!(body["deciders"], json!(["editor@example.org"]));
    assert_eq!(
        body["proposers"],
        json!(["editor@example.org", "owner@example.org"])
    );

    let (status, _) = fixture.round(&viewer, &room, new_round()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, list) = fixture
        .send(
            Method::GET,
            &format!("/api/rooms/{room}/rounds"),
            &viewer,
            None,
        )
        .await;
    assert_eq!(
        (status, list.as_array().map(Vec::len)),
        (StatusCode::OK, Some(1))
    );
}

#[tokio::test]
async fn a_bad_budget_is_refused_with_its_reason() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let mut bad = budget();
    bad["cocat:cap"]["qudt:hasUnit"]["skos:notation"] = json!("MiB");

    let (status, body) = fixture
        .round(
            &owner,
            &room,
            json!({"name": "R", "visibility": "open", "budget": bad}),
        )
        .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["detail"].as_str().unwrap().contains("MiB"), "{body}");
}

#[tokio::test]
async fn the_budget_comes_back_as_json_ld() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;

    let (status, body) = fixture
        .round(
            &owner,
            &room,
            json!({"name": "R", "visibility": "open", "budget": budget()}),
        )
        .await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["budget"]["@type"], "cocat:Budget");
    assert_eq!(body["budget"]["cocat:attribute"], "mode");
    let id = body["id"].as_str().unwrap();
    let (_, again) = fixture
        .send(
            Method::GET,
            &format!("/api/rooms/{room}/rounds/{id}"),
            &owner,
            None,
        )
        .await;
    assert_eq!(again["budget"], body["budget"]);
}

#[tokio::test]
async fn a_decider_must_be_an_owner_or_editor() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    fixture
        .editor(&room, "viewer@example.org", Role::Viewer)
        .await;
    let (_, body) = fixture.round(&owner, &room, new_round()).await;
    let path = format!(
        "/api/rooms/{room}/rounds/{}/members",
        body["id"].as_str().unwrap()
    );
    let members = |deciders: Value| json!({"proposers": ["owner@example.org"], "voters": ["viewer@example.org"], "deciders": deciders});

    let (status, _) = fixture
        .send(
            Method::PUT,
            &path,
            &owner,
            Some(members(json!(["viewer@example.org"]))),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = fixture
        .send(
            Method::PUT,
            &path,
            &owner,
            Some(members(json!(["stranger@example.org"]))),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, body) = fixture
        .send(
            Method::PUT,
            &path,
            &owner,
            Some(members(json!(["owner@example.org"]))),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["voters"], json!(["viewer@example.org"]));
}

#[tokio::test]
async fn deciding_needs_an_existing_catalogue() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    fixture.seed_catalogue(&room);
    let (_, body) = fixture.round(&owner, &room, new_round()).await;
    let round = body["id"].as_str().unwrap();

    let (status, _) = fixture
        .move_to(&owner, &room, round, json!({"state": "decided"}))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let missing =
        json!({"state": "decided", "final_catalogue": "bbbbbbbb-0000-4000-8000-000000000002"});
    let (status, _) = fixture.move_to(&owner, &room, round, missing).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let good = json!({"state": "decided", "final_catalogue": CATALOGUE});
    let (status, body) = fixture.move_to(&owner, &room, round, good).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "decided");
    assert_eq!(body["final_catalogue"], CATALOGUE);
}

#[tokio::test]
async fn deciding_stamps_the_round_on_the_final_catalogue() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    fixture.seed_catalogue(&room);
    fixture.editor(&room, "ada@example.org", Role::Editor).await;
    let body = json!({"name": "Round 1", "visibility": "open", "budget": budget()});
    let (_, created) = fixture.round(&owner, &room, body).await;
    let round = created["id"].as_str().unwrap();
    let members =
        json!({"proposers": ["ada@example.org"], "voters": [], "deciders": ["owner@example.org"]});
    let path = format!("/api/rooms/{room}/rounds/{round}/members");
    fixture
        .send(Method::PUT, &path, &owner, Some(members))
        .await;

    let good = json!({"state": "decided", "final_catalogue": CATALOGUE});
    let (status, _) = fixture.move_to(&owner, &room, round, good).await;
    assert_eq!(status, StatusCode::OK);

    let path = format!("/api/rooms/{room}/catalogues/{CATALOGUE}");
    let (_, catalogue) = fixture.send(Method::GET, &path, &owner, None).await;
    let stamp: Value =
        serde_json::from_str(catalogue["attributes"]["round"].as_str().unwrap()).unwrap();
    assert_eq!(stamp["@type"], "cocat:Round");
    assert_eq!(stamp["cocat:name"], "Round 1");
    assert_eq!(stamp["cocat:round"], round);
    assert_eq!(stamp["cocat:decidedBy"], "owner@example.org");
    assert_eq!(stamp["cocat:proposers"], json!(["ada@example.org"]));
    assert_eq!(
        stamp["cocat:budget"]["cocat:cap"]["qudt:numericValue"],
        10.0
    );
    assert!(stamp["cocat:decidedAt"].is_string());
}

#[tokio::test]
async fn an_owner_moves_a_round_back_and_it_is_logged() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    fixture.seed_catalogue(&room);
    let (_, body) = fixture.round(&owner, &room, new_round()).await;
    let round = body["id"].as_str().unwrap();
    let good = json!({"state": "decided", "final_catalogue": CATALOGUE});
    fixture.move_to(&owner, &room, round, good).await;

    let (status, body) = fixture
        .move_to(&owner, &room, round, json!({"state": "proposing"}))
        .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "proposing");
    let last = body["log"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["what"], "state: proposing");
    assert_eq!(last["actor"], "owner@example.org");
}

#[tokio::test]
async fn an_editor_decider_cannot_move_back() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    fixture.seed_catalogue(&room);
    let (_, editor) = fixture
        .editor(&room, "editor@example.org", Role::Editor)
        .await;
    let (_, body) = fixture.round(&editor, &room, new_round()).await;
    let round = body["id"].as_str().unwrap();
    let good = json!({"state": "decided", "final_catalogue": CATALOGUE});
    let (status, _) = fixture.move_to(&editor, &room, round, good).await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = fixture
        .move_to(&editor, &room, round, json!({"state": "proposing"}))
        .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_decided_round_refuses_member_changes_too() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    fixture.seed_catalogue(&room);
    let (_, body) = fixture.round(&owner, &room, new_round()).await;
    let round = body["id"].as_str().unwrap();
    let good = json!({"state": "decided", "final_catalogue": CATALOGUE});
    fixture.move_to(&owner, &room, round, good).await;
    let members = json!({"proposers": [], "voters": [], "deciders": ["owner@example.org"]});

    let path = format!("/api/rooms/{room}/rounds/{round}/members");
    let (status, _) = fixture
        .send(Method::PUT, &path, &owner, Some(members))
        .await;

    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_deadline_is_stored_in_utc() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;

    let body = json!({"name": "R", "visibility": "open", "deadline": "2026-11-12T12:00:00+02:00"});
    let (status, created) = fixture.round(&owner, &room, body).await;

    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["deadline"], "2026-11-12T10:00:00Z");
    let id = created["id"].as_str().unwrap();
    let path = format!("/api/rooms/{room}/rounds/{id}");
    let change = json!({"deadline": "2026-11-13T00:30:00+01:00"});
    let (_, patched) = fixture
        .send(Method::PATCH, &path, &owner, Some(change))
        .await;
    assert_eq!(patched["deadline"], "2026-11-12T23:30:00Z");
}

#[tokio::test]
async fn a_repeated_member_address_is_kept_once() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let (_, body) = fixture.round(&owner, &room, new_round()).await;
    let path = format!(
        "/api/rooms/{room}/rounds/{}/members",
        body["id"].as_str().unwrap()
    );
    let twice = json!(["owner@example.org", "owner@example.org"]);

    let members = json!({"proposers": twice, "voters": twice, "deciders": twice});
    let (status, body) = fixture
        .send(Method::PUT, &path, &owner, Some(members))
        .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["deciders"], json!(["owner@example.org"]));
}

const EVENT_A: &str = "bbbbbbbb-0000-4000-8000-00000000000a";
const EVENT_B: &str = "bbbbbbbb-0000-4000-8000-00000000000b";

impl Fixture {
    async fn voting_round(&self, owner: &str, room: &str, visibility: &str) -> String {
        let body = json!({"name": "Round 1", "visibility": visibility});
        let (_, created) = self.round(owner, room, body).await;
        let round = created["id"].as_str().unwrap().to_owned();
        self.move_to(owner, room, &round, json!({"state": "voting"}))
            .await;
        round
    }

    async fn put(&self, token: &str, path: String, body: Value) -> (StatusCode, Value) {
        self.send(Method::PUT, &path, token, Some(body)).await
    }

    async fn vote(&self, token: &str, room: &str, round: &str, events: &[&str]) -> StatusCode {
        let path = format!("/api/rooms/{room}/rounds/{round}/votes");
        self.put(token, path, json!({"events": events})).await.0
    }

    async fn verdicts(&self, token: &str, room: &str, round: &str, set: Value) -> StatusCode {
        let path = format!("/api/rooms/{room}/rounds/{round}/verdicts");
        self.put(token, path, json!({"verdicts": set})).await.0
    }

    async fn verdict(
        &self,
        token: &str,
        room: &str,
        round: &str,
        event: &str,
        v: Value,
    ) -> StatusCode {
        let path = format!("/api/rooms/{room}/rounds/{round}/verdicts/{event}");
        self.put(token, path, json!({"verdict": v})).await.0
    }

    async fn read_verdicts(&self, token: &str, room: &str, round: &str) -> (StatusCode, Value) {
        let path = format!("/api/rooms/{room}/rounds/{round}/verdicts");
        self.send(Method::GET, &path, token, None).await
    }

    async fn read_votes(&self, token: &str, room: &str, round: &str) -> (StatusCode, Value) {
        let path = format!("/api/rooms/{room}/rounds/{round}/votes");
        self.send(Method::GET, &path, token, None).await
    }

    async fn two_reviewers(
        &self,
        owner: &str,
        room: &str,
        visibility: &str,
    ) -> (String, String, String) {
        let (_, voter) = self.editor(room, "voter@example.org", Role::Editor).await;
        let (_, other) = self.editor(room, "other@example.org", Role::Editor).await;
        let round = self.voting_round(owner, room, visibility).await;
        let members = json!({"proposers": [], "voters": ["voter@example.org", "other@example.org"], "deciders": ["owner@example.org"]});
        let path = format!("/api/rooms/{room}/rounds/{round}/members");
        self.send(Method::PUT, &path, owner, Some(members)).await;
        (round, voter, other)
    }
}

fn counts(events: &Value, event: &str) -> Value {
    let found = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"] == event);
    let found = found.unwrap_or(&Value::Null);
    json!([found["accept"], found["reject"], found["unsure"]])
}

#[tokio::test]
async fn a_reviewer_sets_one_verdict_and_changes_it() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let round = fixture.voting_round(&owner, &room, "open").await;

    let set = |value| fixture.verdict(&owner, &room, &round, EVENT_A, value);
    assert_eq!(set(json!("reject")).await, StatusCode::OK);
    assert_eq!(set(json!("accept")).await, StatusCode::OK);

    let (status, seen) = fixture.read_verdicts(&owner, &room, &round).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seen["revision"], 1);
    assert_eq!(counts(&seen, EVENT_A), json!([1, 0, 0]));
    let by = &seen["events"][0]["by"];
    assert_eq!(
        by,
        &json!([{"reviewer": "owner@example.org", "verdict": "accept", "revision": 1}])
    );

    assert_eq!(set(json!(null)).await, StatusCode::OK);
    let (_, seen) = fixture.read_verdicts(&owner, &room, &round).await;
    assert_eq!(seen["events"], json!([]));
}

#[tokio::test]
async fn bulk_verdicts_merge() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let round = fixture.voting_round(&owner, &room, "open").await;
    fixture
        .verdicts(
            &owner,
            &room,
            &round,
            json!({EVENT_A: "accept", EVENT_B: "unsure"}),
        )
        .await;

    let status = fixture
        .verdicts(&owner, &room, &round, json!({EVENT_B: "reject"}))
        .await;

    assert_eq!(status, StatusCode::OK);
    let (_, seen) = fixture.read_verdicts(&owner, &room, &round).await;
    assert_eq!(counts(&seen, EVENT_A), json!([1, 0, 0]));
    assert_eq!(counts(&seen, EVENT_B), json!([0, 1, 0]));
}

#[tokio::test]
async fn the_old_vote_call_sets_accepts_and_keeps_rejects() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let round = fixture.voting_round(&owner, &room, "open").await;
    let event_c = "bbbbbbbb-0000-4000-8000-00000000000c";
    fixture
        .verdicts(
            &owner,
            &room,
            &round,
            json!({EVENT_A: "accept", EVENT_B: "reject", event_c: "unsure"}),
        )
        .await;

    assert_eq!(
        fixture.vote(&owner, &room, &round, &[event_c]).await,
        StatusCode::OK
    );

    let (_, seen) = fixture.read_verdicts(&owner, &room, &round).await;
    assert_eq!(counts(&seen, EVENT_A), json!([null, null, null]));
    assert_eq!(counts(&seen, EVENT_B), json!([0, 1, 0]));
    assert_eq!(counts(&seen, event_c), json!([1, 0, 0]));
    let (_, votes) = fixture.read_votes(&owner, &room, &round).await;
    assert_eq!(
        votes,
        json!([{"voter": "owner@example.org", "events": [event_c]}])
    );
}

#[tokio::test]
async fn verdicts_are_refused_outside_voting_and_to_non_voters() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let (_, created) = fixture.round(&owner, &room, new_round()).await;
    let round = created["id"].as_str().unwrap();
    let forbidden = StatusCode::FORBIDDEN;
    assert_eq!(
        fixture
            .verdict(&owner, &room, round, EVENT_A, json!("accept"))
            .await,
        forbidden
    );

    fixture
        .move_to(&owner, &room, round, json!({"state": "voting"}))
        .await;
    let (_, editor) = fixture
        .editor(&room, "editor@example.org", Role::Editor)
        .await;
    let members = json!({"proposers": [], "voters": ["owner@example.org"], "deciders": ["owner@example.org"]});
    let path = format!("/api/rooms/{room}/rounds/{round}/members");
    fixture
        .send(Method::PUT, &path, &owner, Some(members))
        .await;
    assert_eq!(
        fixture
            .verdicts(&editor, &room, round, json!({EVENT_A: "accept"}))
            .await,
        forbidden,
        "not a voter of the round"
    );
    assert_eq!(
        fixture
            .verdict(&owner, &room, round, "not-a-uuid", json!("accept"))
            .await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        fixture
            .verdicts(&owner, &room, round, json!({"nope": "accept"}))
            .await,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    fixture
        .move_to(&owner, &room, round, json!({"state": "proposing"}))
        .await;
    assert_eq!(
        fixture
            .verdict(&owner, &room, round, EVENT_A, json!("accept"))
            .await,
        forbidden
    );
}

#[tokio::test]
async fn blind_verdicts_show_only_to_the_reviewer_deciders_and_owners() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let (round, voter, other) = fixture.two_reviewers(&owner, &room, "blind").await;
    fixture
        .verdict(&voter, &room, &round, EVENT_A, json!("accept"))
        .await;
    fixture
        .verdict(&other, &room, &round, EVENT_A, json!("reject"))
        .await;
    let reviewers = |seen: &Value| {
        seen["events"][0]["by"]
            .as_array()
            .map(|by| by.iter().map(|b| b["reviewer"].clone()).collect::<Vec<_>>())
    };

    let (_, seen) = fixture.read_verdicts(&voter, &room, &round).await;
    assert_eq!(reviewers(&seen), Some(vec![json!("voter@example.org")]));
    assert_eq!(
        counts(&seen, EVENT_A),
        json!([1, 0, 0]),
        "counts hide what the reviewer may not read"
    );
    let (_, seen) = fixture.read_verdicts(&owner, &room, &round).await;
    assert_eq!(reviewers(&seen).map(|by| by.len()), Some(2));

    fixture.seed_catalogue(&room);
    let decided = json!({"state": "decided", "final_catalogue": CATALOGUE});
    fixture.move_to(&owner, &room, &round, decided).await;
    let (_, seen) = fixture.read_verdicts(&voter, &room, &round).await;
    assert_eq!(reviewers(&seen).map(|by| by.len()), Some(2));
}

#[tokio::test]
async fn a_new_revision_restarts_progress() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let round = fixture.voting_round(&owner, &room, "open").await;
    fixture
        .verdict(&owner, &room, &round, EVENT_A, json!("accept"))
        .await;

    fixture
        .move_to(&owner, &room, &round, json!({"state": "proposing"}))
        .await;
    fixture
        .move_to(&owner, &room, &round, json!({"state": "voting"}))
        .await;

    let (_, seen) = fixture.read_verdicts(&owner, &room, &round).await;
    assert_eq!(seen["revision"], 2);
    assert_eq!(counts(&seen, EVENT_A), json!([0, 0, 0]));
    assert_eq!(seen["events"][0]["by"][0]["revision"], 1);
    fixture
        .verdict(&owner, &room, &round, EVENT_A, json!("reject"))
        .await;
    let (_, seen) = fixture.read_verdicts(&owner, &room, &round).await;
    assert_eq!(counts(&seen, EVENT_A), json!([0, 1, 0]));
}

#[tokio::test]
async fn a_voter_replaces_their_votes() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let round = fixture.voting_round(&owner, &room, "open").await;

    assert_eq!(
        fixture
            .vote(&owner, &room, &round, &[EVENT_A, EVENT_B])
            .await,
        StatusCode::OK
    );
    assert_eq!(
        fixture.vote(&owner, &room, &round, &[EVENT_B]).await,
        StatusCode::OK
    );

    let (status, votes) = fixture.read_votes(&owner, &room, &round).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        votes,
        json!([{"voter": "owner@example.org", "events": [EVENT_B]}])
    );
}

#[tokio::test]
async fn votes_are_refused_outside_voting() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let (_, created) = fixture.round(&owner, &room, new_round()).await;
    let round = created["id"].as_str().unwrap();
    assert_eq!(
        fixture.vote(&owner, &room, round, &[EVENT_A]).await,
        StatusCode::FORBIDDEN
    );

    fixture
        .move_to(&owner, &room, round, json!({"state": "voting"}))
        .await;
    let (_, editor) = fixture
        .editor(&room, "editor@example.org", Role::Editor)
        .await;
    let members = json!({"proposers": [], "voters": ["owner@example.org"], "deciders": ["owner@example.org"]});
    let path = format!("/api/rooms/{room}/rounds/{round}/members");
    fixture
        .send(Method::PUT, &path, &owner, Some(members))
        .await;
    assert_eq!(
        fixture.vote(&editor, &room, round, &[EVENT_A]).await,
        StatusCode::FORBIDDEN,
        "not a voter of the round"
    );

    fixture
        .move_to(&owner, &room, round, json!({"state": "proposing"}))
        .await;
    assert_eq!(
        fixture.vote(&owner, &room, round, &[EVENT_A]).await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn blind_votes_show_only_to_the_voter_and_deciders() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let (_, voter) = fixture
        .editor(&room, "voter@example.org", Role::Editor)
        .await;
    let (_, other) = fixture
        .editor(&room, "other@example.org", Role::Editor)
        .await;
    let round = fixture.voting_round(&owner, &room, "blind").await;
    let members = json!({"proposers": [], "voters": ["voter@example.org", "other@example.org"], "deciders": ["owner@example.org"]});
    let path = format!("/api/rooms/{room}/rounds/{round}/members");
    fixture
        .send(Method::PUT, &path, &owner, Some(members))
        .await;
    fixture.vote(&voter, &room, &round, &[EVENT_A]).await;
    fixture.vote(&other, &room, &round, &[EVENT_B]).await;
    let only = |email: &str, event: &str| json!([{"voter": email, "events": [event]}]);

    let (_, seen) = fixture.read_votes(&voter, &room, &round).await;
    assert_eq!(seen, only("voter@example.org", EVENT_A));
    let (_, seen) = fixture.read_votes(&owner, &room, &round).await;
    assert_eq!(seen.as_array().map(Vec::len), Some(2));

    fixture.seed_catalogue(&room);
    let decided = json!({"state": "decided", "final_catalogue": CATALOGUE});
    fixture.move_to(&owner, &room, &round, decided).await;
    let (_, seen) = fixture.read_votes(&voter, &room, &round).await;
    assert_eq!(seen.as_array().map(Vec::len), Some(2));
}

#[tokio::test]
async fn open_votes_show_to_every_member() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let (_, viewer) = fixture
        .editor(&room, "viewer@example.org", Role::Viewer)
        .await;
    let round = fixture.voting_round(&owner, &room, "open").await;
    fixture.vote(&owner, &room, &round, &[EVENT_A]).await;

    let (status, seen) = fixture.read_votes(&viewer, &room, &round).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        seen,
        json!([{"voter": "owner@example.org", "events": [EVENT_A]}])
    );
}

#[tokio::test]
async fn a_round_moved_back_shows_no_final_catalogue() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    fixture.seed_catalogue(&room);
    let (_, body) = fixture.round(&owner, &room, new_round()).await;
    let round = body["id"].as_str().unwrap();
    let good = json!({"state": "decided", "final_catalogue": CATALOGUE});
    let (_, decided) = fixture.move_to(&owner, &room, round, good).await;
    assert_eq!(decided["final_catalogue"], CATALOGUE);

    let (_, body) = fixture
        .move_to(&owner, &room, round, json!({"state": "proposing"}))
        .await;

    assert_eq!(body["final_catalogue"], Value::Null);
}

#[tokio::test]
async fn a_decider_demoted_to_viewer_loses_the_decider_powers() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let (editor_id, editor) = fixture
        .editor(&room, "editor@example.org", Role::Editor)
        .await;
    let (_, body) = fixture.round(&editor, &room, new_round()).await;
    let round = body["id"].as_str().unwrap();
    db::set_role(&fixture.state.pool, &room, editor_id, Role::Viewer)
        .await
        .unwrap();

    let (moved, _) = fixture
        .move_to(&editor, &room, round, json!({"state": "voting"}))
        .await;
    let path = format!("/api/rooms/{room}/rounds/{round}");
    let (patched, _) = fixture
        .send(Method::PATCH, &path, &editor, Some(json!({"name": "Mine"})))
        .await;
    let (seated, _) = fixture
        .send(
            Method::PUT,
            &format!("{path}/members"),
            &editor,
            Some(json!({"proposers": [], "voters": [], "deciders": ["editor@example.org"]})),
        )
        .await;

    assert_eq!(
        (moved, patched, seated),
        (
            StatusCode::FORBIDDEN,
            StatusCode::FORBIDDEN,
            StatusCode::FORBIDDEN
        )
    );
}

#[tokio::test]
async fn a_kind_is_set_at_creation_and_changed_by_patch() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;

    let (_, plain) = fixture.round(&owner, &room, new_round()).await;
    let with_budget = json!({"name": "R", "visibility": "open", "budget": budget()});
    let (_, budgeted) = fixture.round(&owner, &room, with_budget).await;
    let downlink = json!({"name": "R", "visibility": "open", "kind": "downlink"});
    let (_, downlinked) = fixture.round(&owner, &room, downlink).await;
    let id = plain["id"].as_str().unwrap();
    let path = format!("/api/rooms/{room}/rounds/{id}");
    let (_, patched) = fixture
        .send(
            Method::PATCH,
            &path,
            &owner,
            Some(json!({"kind": "review"})),
        )
        .await;

    let kinds = [&plain, &budgeted, &downlinked, &patched].map(|round| round["kind"].clone());
    assert_eq!(kinds, ["labelling", "downlink", "downlink", "review"]);
    assert_eq!(plain["revision"], 1);
}

#[tokio::test]
async fn a_note_and_a_revision_show_in_the_round() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let (_, body) = fixture.round(&owner, &room, new_round()).await;
    let round = body["id"].as_str().unwrap();

    let forward = json!({"state": "voting", "note": "Please check the stops"});
    fixture.move_to(&owner, &room, round, forward).await;
    let (_, back) = fixture
        .move_to(&owner, &room, round, json!({"state": "proposing"}))
        .await;

    assert_eq!(back["revision"], 2);
    assert_eq!(back["log"][0]["note"], "Please check the stops");
    assert_eq!(back["log"][1]["note"], Value::Null);
}

#[tokio::test]
async fn a_too_long_note_is_refused() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let (_, body) = fixture.round(&owner, &room, new_round()).await;
    let round = body["id"].as_str().unwrap();

    let long = json!({"state": "voting", "note": "x".repeat(2001)});
    let (refused, _) = fixture.move_to(&owner, &room, round, long).await;
    let fits = json!({"state": "voting", "note": "x".repeat(2000)});
    let (accepted, _) = fixture.move_to(&owner, &room, round, fits).await;

    assert_eq!(
        (refused, accepted),
        (StatusCode::UNPROCESSABLE_ENTITY, StatusCode::OK)
    );
}

const DETECTIONS: &str = "cccccccc-0000-4000-8000-000000000003";
const FIRST: &str = "eeeeeeee-0000-4000-8000-000000000001";
const SECOND: &str = "eeeeeeee-0000-4000-8000-000000000002";
const TRASHED: &str = "eeeeeeee-0000-4000-8000-000000000003";
const LATER: &str = "eeeeeeee-0000-4000-8000-000000000004";

fn at(text: &str) -> Timestamp {
    Timestamp::parse(text).unwrap()
}

/// A catalogue "detections" listing `events`; the `true` ones are in the trash.
fn detections(events: &[(&str, bool)]) -> Vec<u8> {
    let doc = Doc::new();
    {
        let mut txn = doc.transact_mut();
        let catalogue = NewCatalogue {
            uuid: DETECTIONS.to_owned(),
            name: "detections".to_owned(),
            ..NewCatalogue::default()
        };
        model::create_catalogue(&mut txn, &catalogue).unwrap();
        for (uuid, _) in events {
            let record = EventRecord::new(
                *uuid,
                at("2020-01-01T00:00:00Z"),
                at("2020-01-01T01:00:00Z"),
            );
            model::create_event(&mut txn, &record).unwrap();
        }
        let uuids: Vec<String> = events.iter().map(|(uuid, _)| (*uuid).to_owned()).collect();
        model::add_catalogue_events(&mut txn, DETECTIONS, &uuids);
        for (uuid, _) in events.iter().filter(|(_, trashed)| *trashed) {
            model::delete_event(&mut txn, uuid);
        }
    }
    doc.transact()
        .encode_state_as_update_v1(&StateVector::default())
}

/// What a room holds: its catalogues and the events outside the trash.
struct Held {
    catalogues: Vec<model::CatalogueSummary>,
    events: Vec<String>,
    trash: usize,
}

impl Fixture {
    async fn write_room(&self, key: &str, author: Uuid, update: Vec<u8>) {
        let allowance = self
            .state
            .rooms
            .allowance(&self.state.pool, key)
            .await
            .unwrap();
        self.state
            .rooms
            .write_as(key, allowance, Author::Client(author), update)
            .await
            .unwrap();
    }

    /// Adds LATER to the detections of `room`, on top of what it holds.
    async fn add_later_detection(&self, room: &str, author: Uuid) {
        let held = self
            .state
            .rooms
            .read(room, |doc| {
                doc.transact()
                    .encode_state_as_update_v1(&StateVector::default())
            })
            .await
            .unwrap();
        let doc = Doc::new();
        let mut txn = doc.transact_mut();
        txn.apply_update(Update::decode_v1(&held).unwrap()).unwrap();
        let before = txn.state_vector();
        let record = EventRecord::new(
            LATER,
            at("2020-01-02T00:00:00Z"),
            at("2020-01-02T01:00:00Z"),
        );
        model::create_event(&mut txn, &record).unwrap();
        model::add_catalogue_events(&mut txn, DETECTIONS, &[LATER.to_owned()]);
        let update = txn.encode_state_as_update_v1(&before);
        drop(txn);
        self.write_room(room, author, update).await;
    }

    async fn held(&self, key: &str) -> Held {
        self.state
            .rooms
            .read(key, |doc| {
                let txn = doc.transact();
                let catalogues = model::catalogues(&txn);
                let mut events: Vec<String> = catalogues
                    .iter()
                    .flat_map(|found| model::listed_events(&txn, &found.uuid))
                    .filter(|uuid| model::event(&txn, uuid).is_some())
                    .collect();
                events.sort();
                Held {
                    catalogues,
                    events,
                    trash: model::trash(&txn).len(),
                }
            })
            .await
            .unwrap()
    }

    async fn review_round(&self, token: &str, room: &str, source: &str) -> (StatusCode, Value) {
        let body = json!({
            "name": "Review",
            "visibility": "open",
            "kind": "review",
            "candidates": {"project": source, "catalogue": DETECTIONS},
        });
        self.round(token, room, body).await
    }
}

fn candidates_key(room: &str, round: &Value) -> String {
    format!("{room}/{}", round["candidates"]["doc"].as_str().unwrap())
}

#[tokio::test]
async fn a_review_round_copies_its_candidates_and_starts_reviewing() {
    let fixture = Fixture::start().await;
    let (owner_id, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let events = [(FIRST, false), (SECOND, false)];
    fixture
        .write_room(&room, owner_id, detections(&events))
        .await;

    let (status, round) = fixture.review_round(&owner, &room, &room).await;

    assert_eq!(status, StatusCode::CREATED, "{round}");
    assert_eq!(round["state"], "voting");
    assert_eq!(round["proposers"], json!([]));
    assert_eq!(round["voters"], json!(["owner@example.org"]));
    assert_eq!(round["proposals"], json!([]));
    let candidates = &round["candidates"];
    assert_eq!(candidates["count"], 2);
    assert_eq!(
        candidates["source"],
        json!({"project": room, "catalogue": DETECTIONS})
    );
    assert!(candidates["doc"].is_string());
    let last = round["log"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["what"], "state: voting");
    assert_eq!(last["actor"], "owner@example.org");
}

#[tokio::test]
async fn candidates_keep_the_source_ids_and_name_their_source() {
    let fixture = Fixture::start().await;
    let (owner_id, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let events = [(FIRST, false), (SECOND, false)];
    fixture
        .write_room(&room, owner_id, detections(&events))
        .await;

    let (_, round) = fixture.review_round(&owner, &room, &room).await;

    let held = fixture.held(&candidates_key(&room, &round)).await;
    assert_eq!(held.events, [FIRST, SECOND]);
    let [catalogue] = held.catalogues.as_slice() else {
        panic!("expected one catalogue, got {:?}", held.catalogues);
    };
    assert_eq!(
        (catalogue.uuid.as_str(), catalogue.name.as_str()),
        (DETECTIONS, "detections")
    );
    let hop = catalogue.copied_from.last().unwrap();
    assert_eq!(hop.project, room);
    assert_eq!(hop.catalogue, DETECTIONS);
    assert_eq!(hop.by.as_deref(), Some("owner@example.org"));
}

#[tokio::test]
async fn candidates_are_a_snapshot() {
    let fixture = Fixture::start().await;
    let (owner_id, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    fixture
        .write_room(&room, owner_id, detections(&[(FIRST, false)]))
        .await;
    let (_, round) = fixture.review_round(&owner, &room, &room).await;

    fixture.add_later_detection(&room, owner_id).await;

    assert_eq!(fixture.held(&room).await.events, [FIRST, LATER]);
    let held = fixture.held(&candidates_key(&room, &round)).await;
    assert_eq!(held.events, [FIRST]);
    let id = round["id"].as_str().unwrap();
    let path = format!("/api/rooms/{room}/rounds/{id}");
    let (_, again) = fixture.send(Method::GET, &path, &owner, None).await;
    assert_eq!(again["candidates"]["count"], 1);
}

#[tokio::test]
async fn trashed_source_events_are_not_candidates() {
    let fixture = Fixture::start().await;
    let (owner_id, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let events = [(FIRST, false), (TRASHED, true), (SECOND, false)];
    fixture
        .write_room(&room, owner_id, detections(&events))
        .await;

    let (_, round) = fixture.review_round(&owner, &room, &room).await;

    assert_eq!(round["candidates"]["count"], 2);
    let held = fixture.held(&candidates_key(&room, &round)).await;
    assert_eq!(held.events, [FIRST, SECOND]);
    assert_eq!(held.trash, 0);
}

#[tokio::test]
async fn a_source_the_creator_cannot_read_is_refused() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let (stranger_id, stranger) = fixture.user_token("stranger@example.org").await;
    let theirs = fixture.project(&stranger, "catalogues").await;
    fixture
        .write_room(&theirs, stranger_id, detections(&[(FIRST, false)]))
        .await;

    let (status, body) = fixture.review_round(&owner, &room, &theirs).await;
    let (missing, _) = fixture.review_round(&owner, &room, &room).await;

    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(missing, StatusCode::NOT_FOUND);
    let (_, list) = fixture
        .send(
            Method::GET,
            &format!("/api/rooms/{room}/rounds"),
            &owner,
            None,
        )
        .await;
    assert_eq!(list, json!([]));
}

#[tokio::test]
async fn candidates_come_from_a_public_catalogue_too() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let (curator_id, _) = fixture.user_token("curator@example.org").await;
    let public = "public.detections";
    let made = db::create_public_room(
        &fixture.state.pool,
        &db::PublicRoom {
            id: public.to_owned(),
            name: "Detections".to_owned(),
            folder: None,
            source: None,
            description: None,
            public_count: None,
            counted_file: None,
        },
        Some(curator_id),
    )
    .await
    .unwrap();
    assert!(made);
    fixture
        .write_room(public, curator_id, detections(&[(FIRST, false)]))
        .await;

    let (status, round) = fixture.review_round(&owner, &room, public).await;

    assert_eq!(status, StatusCode::CREATED, "{round}");
    assert_eq!(round["candidates"]["count"], 1);
}

#[tokio::test]
async fn a_review_round_without_candidates_is_refused_with_a_sentence() {
    let fixture = Fixture::start().await;
    let (_, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    let body = json!({"name": "Review", "visibility": "open", "kind": "review"});

    let (status, answer) = fixture.round(&owner, &room, body).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{answer}");
    assert_eq!(
        answer["detail"],
        "A review round needs candidates: pick a catalogue."
    );
}

#[tokio::test]
async fn the_document_list_names_candidates_as_candidates() {
    let fixture = Fixture::start().await;
    let (owner_id, owner) = fixture.user_token("owner@example.org").await;
    let room = fixture.project(&owner, "catalogues").await;
    fixture
        .write_room(&room, owner_id, detections(&[(FIRST, false)]))
        .await;
    let (_, round) = fixture.review_round(&owner, &room, &room).await;
    let doc = round["candidates"]["doc"].as_str().unwrap().to_owned();

    let (_, listed) = fixture
        .send(
            Method::GET,
            &format!("/api/rooms/{room}/documents"),
            &owner,
            None,
        )
        .await;

    let found = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|document| document["id"] == doc)
        .unwrap();
    assert_eq!(found["kind"], "candidates");
}
