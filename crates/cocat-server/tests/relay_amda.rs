//! Upgrading a server where `tools/amda_public.py` already made AMDA's public catalogues: the
//! AMDA provider adopts them as they are. The provider reads saved AMDA answers here, never
//! the network.

use std::fs;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use cocat_core::model::{self, EventRecord, NewCatalogue};
use cocat_core::time::Timestamp;
use cocat_server::relay::amda::{self, SharedList};
use cocat_server::relay::{self, Fetched, Provider};
use cocat_server::room::{Author, RoomManager};
use cocat_server::{AppState, auth, db, links::Links, ratelimit::LoginLimit, ws};
use serde_json::{Map, Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;
use yrs::{Doc, Transact};

const CATALOGS: &str = include_str!("fixtures/relay/amda_catalogs.xml");
const TIME_TABLES: &str = include_str!("fixtures/relay/amda_timetables.xml");
const SOLO: &str = include_str!("fixtures/relay/amda_sharedcatalog_63.xml");
const SUBSTORMS: &str = include_str!("fixtures/relay/amda_sharedtimeTable_10.xml");

const SOLO_ROOM: &str = "public.amda.sharedcatalog_63";
const SUBSTORMS_ROOM: &str = "public.amda.sharedtimeTable_10";
const SOLO_DESCRIPTION: &str = "Solar Orbiter is a Sun-observing satellite, under development by the European Space Agency. The mission was launched with an Atlas V from the Cape Canaveral AFS in Florida on 09 February 2020. This catalog has been created using spice kernels provided by spdf and the JPL's orbnum tool (https://naif.jpl.nasa.gov/pub/naif/utilities/PC_Linux_64bit/orbnum.ug).";
const SUBSTORMS_DESCRIPTION: &str = "THEMIS substorms list based on overview jpegs at station CHBG Substorm identification is done purely by visible inspection of all-sky images  There is no guarantee that a certain feature is really a substorm  Please see ftp://justice.ssl.berkeley.edu/events/ for more details; \n        Historic: From old AMDA;\n        Creation Date :  2013-11-22T13:52:49;";

/// The AMDA provider reading the saved answers instead of AMDA.
struct SavedAmda;

#[async_trait]
impl Provider for SavedAmda {
    fn id(&self) -> &'static str {
        "amda"
    }

    fn source(&self) -> &'static str {
        amda::SOURCE
    }

    fn default_schedule(&self) -> &'static str {
        "0 4 * * *"
    }

    async fn fetch(
        &self,
        _http: &reqwest::Client,
        _settings: &Value,
        _last_ok: Option<Timestamp>,
    ) -> Result<Vec<Fetched>, String> {
        Ok(vec![
            amda::fetched_list(&saved(CATALOGS, "sharedcatalog_63"), SOLO)?,
            amda::fetched_list(&saved(TIME_TABLES, "sharedtimeTable_10"), SUBSTORMS)?,
        ])
    }
}

fn saved(tree: &str, id: &str) -> SharedList {
    amda::shared_lists(tree)
        .unwrap()
        .into_iter()
        .find(|list| list.id == id)
        .unwrap()
}

/// What the script wrote for one shared list: its index entry, its one catalogue and the
/// list's intervals, with the uuids and values `tools/amda_public.py` computed for these
/// files.
struct ScriptList {
    room: &'static str,
    amda_id: &'static str,
    name: &'static str,
    folder: &'static str,
    description: &'static str,
    catalogue: &'static str,
    columns: &'static [&'static str],
    events: Vec<(&'static str, &'static str, &'static str, Value)>,
}

fn solo() -> ScriptList {
    let orbit = |n: &str, axis: &str, incl: &str, eccen: &str, lon: &str, peri: &str| {
        json!({"Orbit No.": n, "Semi Axis.": axis, "Incl.": incl, "Eccen.": eccen,
               "Lon Asc.": lon, "Arg. Peri.": peri})
    };
    ScriptList {
        room: SOLO_ROOM,
        amda_id: "sharedcatalog_63",
        name: "solo",
        folder: "AMDA/SPACECRAFT_ORBIT_NUMBERS",
        description: SOLO_DESCRIPTION,
        catalogue: "931dccc5-a77f-5e2b-8702-53b2df03b6ba",
        columns: &[
            "Orbit No.",
            "Semi Axis.",
            "Incl.",
            "Eccen.",
            "Lon Asc.",
            "Arg. Peri.",
        ],
        events: vec![
            (
                "b54195cf-d8f9-5d48-85fa-11ffa44fae65",
                "2020-10-12T11:53:07Z",
                "2021-06-02T18:11:00Z",
                orbit("1", "112546934.18", "22.07", "0.314", "3.06", "323.55"),
            ),
            (
                "46435578-93af-5fc1-8417-87391644c560",
                "2021-06-02T18:11:00Z",
                "2021-12-16T14:25:36Z",
                orbit("2", "108209102.13", "26.76", "0.316", "11.85", "322.41"),
            ),
            (
                "63e3d16b-22c2-5f8f-8683-56688daaa73f",
                "2021-12-16T14:25:36Z",
                "2022-07-04T09:25:03Z",
                orbit("3", "100050395.89", "24.86", "0.518", "6.50", "252.15"),
            ),
        ],
    }
}

fn substorms() -> ScriptList {
    let none = || json!({});
    ScriptList {
        room: SUBSTORMS_ROOM,
        amda_id: "sharedtimeTable_10",
        name: "THEMIS_Substorm_2007-2008_CHBG",
        folder: "AMDA/EARTH",
        description: SUBSTORMS_DESCRIPTION,
        catalogue: "9053fd48-77cc-5d57-ace5-8f05d2b5648d",
        columns: &[],
        events: vec![
            (
                "e04ff100-5dba-5528-86fc-536137c24f96",
                "2007-12-07T04:04:00Z",
                "2007-12-07T04:04:01Z",
                none(),
            ),
            (
                "29557756-170e-5726-9b78-8dcea7cd9c0f",
                "2008-01-06T08:18:00Z",
                "2008-01-06T08:18:01Z",
                none(),
            ),
            (
                "64c3e598-5f93-5b58-980d-a3edc4332173",
                "2008-02-10T05:40:00Z",
                "2008-02-10T05:40:01Z",
                none(),
            ),
            (
                "600ebe10-677c-5c6f-ba34-03a2bdf6737c",
                "2008-03-04T05:38:00Z",
                "2008-03-04T05:38:01Z",
                none(),
            ),
        ],
    }
}

fn at(text: &str) -> Timestamp {
    Timestamp::parse(text).unwrap()
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("not an object"),
    }
}

impl ScriptList {
    /// The room as the script's Python client wrote it: the catalogue and events made by
    /// `AMDA`, with a creation time, the catalogue's columns and its AMDA attributes.
    fn update(&self) -> Vec<u8> {
        let doc = Doc::new();
        let mut txn = doc.transact_mut();
        let made = at("2026-11-01T10:00:00Z");
        let catalogue = NewCatalogue {
            uuid: self.catalogue.to_owned(),
            name: self.name.to_owned(),
            author: Some("AMDA".to_owned()),
            created: Some(made),
            tags: Vec::new(),
            columns: self.columns.iter().map(|c| (*c).to_owned()).collect(),
            attributes: object(json!({
                "description": self.description,
                "amda_id": self.amda_id,
                "source": "AMDA",
            })),
        };
        model::create_catalogue(&mut txn, &catalogue).unwrap();
        let mut uuids = Vec::new();
        for (uuid, start, stop, attributes) in &self.events {
            let mut event = EventRecord::new(*uuid, at(start), at(stop));
            event.author = Some("AMDA".to_owned());
            event.created = Some(made);
            event.attributes = object(attributes.clone());
            model::create_event(&mut txn, &event).unwrap();
            uuids.push((*uuid).to_owned());
        }
        assert!(model::add_catalogue_events(
            &mut txn,
            self.catalogue,
            &uuids
        ));
        txn.encode_update_v1()
    }

    fn entry(&self) -> db::PublicRoom {
        db::PublicRoom {
            id: self.room.to_owned(),
            name: self.name.to_owned(),
            folder: Some(self.folder.to_owned()),
            source: Some("AMDA".to_owned()),
            description: Some(self.description.to_owned()),
            public_count: None,
            counted_file: None,
        }
    }
}

struct World {
    state: AppState,
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
            rooms: RoomManager::new(update_dir, None),
            before_attach: None,
            login_limit: LoginLimit::default(),
            mailer: None,
            links: Links::default(),
            oidc: Vec::new(),
            web_dir: None,
            providers: Arc::new(vec![Arc::new(SavedAmda)]),
            http: reqwest::Client::new(),
        };
        let curator = db::create_user(&state.pool, "curator@example.org", "pwd", false, true)
            .await
            .unwrap();
        db::set_curator(&state.pool, curator, true).await.unwrap();
        Self {
            state,
            curator,
            _dir: dir,
        }
    }

    /// The public catalogue the script made, before the relay knew it.
    async fn made_by_the_script(&self, list: &ScriptList) {
        assert!(
            db::create_public_room(&self.state.pool, &list.entry(), Some(self.curator))
                .await
                .unwrap()
        );
        self.state
            .rooms
            .write_as(
                list.room,
                u64::MAX,
                Author::Client(self.curator),
                list.update(),
            )
            .await
            .unwrap();
    }

    async fn get(&self, path: &str) -> Value {
        let token = auth::start_session(&self.state.pool, self.curator)
            .await
            .unwrap();
        let request = Request::builder()
            .method(Method::GET)
            .uri(path)
            .header(header::COOKIE, format!("fastapiusersauth={token}"))
            .body(Body::empty())
            .unwrap();
        let response = ws::router(self.state.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1 << 22).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// The room's history versions: what each is and changed. Who wrote them is left out, as
    /// adopting makes the curator a reader, who sees authors less.
    async fn versions(&self, room: &str) -> Vec<Value> {
        let history = self.get(&format!("/api/rooms/{room}/history")).await;
        history["versions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|version| {
                let field = |name: &str| version[name].clone();
                json!([
                    field("id"),
                    field("last"),
                    field("ended_ms"),
                    field("events_added"),
                    field("events_changed"),
                    field("events_removed"),
                ])
            })
            .collect()
    }

    /// The index entry as people read it: name, folder, source and description.
    async fn entry(&self, room: &str) -> Value {
        let index = self.get("/api/public").await;
        let row = index
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == room)
            .unwrap()
            .clone();
        json!([
            row["name"],
            row["folder"],
            row["source"],
            row["description"]
        ])
    }
}

#[tokio::test]
async fn a_room_the_script_made_is_adopted_unchanged() {
    let world = World::start().await;
    let lists = [solo(), substorms()];
    for list in &lists {
        world.made_by_the_script(list).await;
    }
    let mut before = Vec::new();
    for list in &lists {
        before.push((
            world.versions(list.room).await,
            world.entry(list.room).await,
        ));
    }

    let report = relay::run(&world.state, "amda").await.unwrap();

    assert_eq!(report.catalogues, 2);
    assert!(report.counts.is_empty(), "{:?}", report.counts);
    for (list, (versions, entry)) in lists.iter().zip(before) {
        assert_eq!(world.versions(list.room).await, versions, "{}", list.room);
        assert_eq!(world.entry(list.room).await, entry, "{}", list.room);
    }
}
