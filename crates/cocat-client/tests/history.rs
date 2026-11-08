mod support;

use cocat_client::{
    LoginError, Room, RoomError, commit, history, name_version, restore, version_state,
};
use serde_json::Value;
use support::*;
use yrs::Transact;

const ROOM: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const START: &str = "2020-01-01T00:00:00Z";
const STOP: &str = "2020-01-01T00:01:00Z";

/// A server whose room holds one version: `a`, written by `owner@x.org`.
async fn one_version() -> (Server, String, u64) {
    let server = Server::start().await;
    let cookie = seeded(&server, ROOM, "owner@x.org").await;
    let dir = cache();
    let room = Room::open(dir.path(), ROOM).unwrap();
    room.connect(&server.url(), &cookie).unwrap();
    room.wait_synced(WAIT).unwrap();
    create_event(&room, "a", START, STOP);
    room.save(WAIT).unwrap();
    room.close();
    let versions = history(&server.url(), &cookie, ROOM, None).await.unwrap();
    let id = versions[0]["id"].as_u64().unwrap();
    (server, cookie, id)
}

#[tokio::test]
async fn history_names_the_writer_and_a_past_room_holds_the_version() {
    let (server, cookie, id) = one_version().await;

    let versions = history(&server.url(), &cookie, ROOM, None).await.unwrap();
    let state = version_state(&server.url(), &cookie, ROOM, id)
        .await
        .unwrap();
    let past = Room::past(ROOM, &state).unwrap();

    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0]["author_email"], "owner@x.org");
    assert!(cocat_core::model::event(&past.doc().transact(), "a").is_some());
    assert!(past.is_past());
    assert!(matches!(
        past.connect(&server.url(), &cookie),
        Err(RoomError::Past)
    ));
}

#[tokio::test]
async fn a_version_takes_a_name_and_restoring_the_present_changes_nothing() {
    let (server, cookie, id) = one_version().await;
    let url = server.url();
    let name = |versions: Vec<Value>| versions[0]["name"].clone();

    name_version(&url, &cookie, ROOM, id, Some("paper v1"))
        .await
        .unwrap();
    assert_eq!(
        name(history(&url, &cookie, ROOM, None).await.unwrap()),
        "paper v1"
    );
    name_version(&url, &cookie, ROOM, id, None).await.unwrap();
    assert_eq!(
        name(history(&url, &cookie, ROOM, None).await.unwrap()),
        Value::Null
    );

    assert!(!restore(&url, &cookie, ROOM, id, None).await.unwrap());
}

#[tokio::test]
async fn a_refused_call_carries_the_status() {
    let (server, cookie, _) = one_version().await;

    let refused = history(&server.url(), &cookie, "no-such-room", None).await;

    assert!(
        matches!(refused, Err(LoginError::Refused(404))),
        "{refused:?}"
    );
}

#[tokio::test]
async fn saving_the_newest_version_returns_its_id_and_its_note() {
    let (server, cookie, id) = one_version().await;
    let url = server.url();

    let saved = commit(&url, &cookie, ROOM, Some("checked"), Some("all good"))
        .await
        .unwrap();
    let again = commit(&url, &cookie, ROOM, None, None).await;

    assert_eq!(saved, id);
    let versions = history(&url, &cookie, ROOM, None).await.unwrap();
    assert_eq!(versions[0]["message"], "all good");
    assert_eq!(versions[0]["name"], "checked");
    assert!(matches!(again, Err(LoginError::Refused(409))), "{again:?}");
}
