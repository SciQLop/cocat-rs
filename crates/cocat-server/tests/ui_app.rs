mod ui_support;

use std::fs;
use std::path::PathBuf;

use axum::http::{StatusCode, header};
use axum::response::Response;
use cocat_core::role::Role;
use cocat_server::links::Links;
use cocat_server::{db, lifecycle};
use ui_support::{Fixture, body_text, location};
use uuid::Uuid;

const OWNER: &str = "owner@example.org";
const VIEWER: &str = "viewer@example.org";
const STRANGER: &str = "stranger@example.org";

const INDEX: &str =
    "<!doctype html>\n<html>\n<head>\n<title>Catalogues</title>\n</head>\n<body></body>\n</html>\n";

/// A bundle as trunk lays it out, and a file beside it that must stay out of reach.
fn bundle(fixture: &Fixture) -> PathBuf {
    let web = fixture.dir().join("web");
    fs::create_dir_all(web.join("snippets")).unwrap();
    fs::write(web.join("index.html"), INDEX).unwrap();
    fs::write(web.join("cocat-web-0123456789abcdef.js"), "export {}").unwrap();
    fs::write(web.join("mark.png"), b"png").unwrap();
    fs::write(fixture.dir().join("secret.txt"), "secret").unwrap();
    web
}

async fn room_with_viewer(fixture: &Fixture) -> (String, Uuid) {
    let owner = fixture.verified(OWNER).await;
    let viewer = fixture.verified(VIEWER).await;
    let room = lifecycle::create_room(&fixture.state, owner, "Solitary <Waves>")
        .await
        .unwrap()
        .id;
    db::add_member(&fixture.state.pool, &room, viewer, Role::Viewer)
        .await
        .unwrap();
    (room, viewer)
}

fn header_of(response: &Response, name: header::HeaderName) -> &str {
    response
        .headers()
        .get(name)
        .map(|value| value.to_str().unwrap())
        .unwrap_or_default()
}

#[tokio::test]
async fn browse_requires_login() {
    let fixture = Fixture::start().await;

    let response = fixture.get("/ui/rooms/lab/browse", None).await;

    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        location(&response),
        "/ui/login?next=%2Fui%2Frooms%2Flab%2Fbrowse"
    );
}

#[tokio::test]
async fn browse_for_a_stranger_is_404() {
    let mut fixture = Fixture::start().await;
    fixture.state.web_dir = Some(bundle(&fixture));
    let (room, _) = room_with_viewer(&fixture).await;
    let stranger = fixture.verified(STRANGER).await;
    let token = fixture.token(stranger).await;

    let response = fixture
        .get(&format!("/ui/rooms/{room}/browse"), Some(&token))
        .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(body_text(response).await.contains("Room not found"));
}

#[tokio::test]
async fn browse_serves_index_with_the_room_meta() {
    let mut fixture = Fixture::start().await;
    fixture.state.web_dir = Some(bundle(&fixture));
    let (room, viewer) = room_with_viewer(&fixture).await;
    let token = fixture.token(viewer).await;

    let response = fixture
        .get(&format!("/ui/rooms/{room}/browse"), Some(&token))
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header_of(&response, header::CACHE_CONTROL), "no-store");
    let body = body_text(response).await;
    for expected in [
        r#"<base href="/ui/app/">"#.to_owned(),
        format!(r#"<meta name="cocat-room" content="{room}">"#),
        r#"<meta name="cocat-room-name" content="Solitary &#60;Waves&#62;">"#.to_owned(),
        r#"<meta name="cocat-role" content="viewer">"#.to_owned(),
        format!(r#"<meta name="cocat-ws" content="/room/{room}">"#),
        r#"<meta name="cocat-api" content="/api">"#.to_owned(),
        format!(r#"<meta name="cocat-user" content="{VIEWER}">"#),
        "<title>Catalogues</title>".to_owned(),
    ] {
        assert!(body.contains(&expected), "{expected} missing from {body}");
    }
    let base = body.find("<base").unwrap();
    assert!(
        base < body.find("<title>").unwrap(),
        "the base must come before anything that loads"
    );
}

#[tokio::test]
async fn behind_a_proxy_the_socket_url_is_public_and_secure() {
    let mut fixture = Fixture::start().await;
    fixture.state.web_dir = Some(bundle(&fixture));
    fixture.state.links = Links::from_env(&|name| match name {
        "COCAT_PUBLIC_URL" => Some("https://cocat.example.org".to_owned()),
        "COCAT_PROXY_PREFIX" => Some("/cocat-dev/".to_owned()),
        _ => None,
    })
    .unwrap();
    let (room, viewer) = room_with_viewer(&fixture).await;
    let token = fixture.token(viewer).await;

    let body = body_text(
        fixture
            .get(&format!("/ui/rooms/{room}/browse"), Some(&token))
            .await,
    )
    .await;

    for expected in [
        r#"<base href="/cocat-dev/ui/app/">"#.to_owned(),
        format!(
            r#"<meta name="cocat-ws" content="wss://cocat.example.org/cocat-dev/room/{room}">"#
        ),
        r#"<meta name="cocat-api" content="/cocat-dev/api">"#.to_owned(),
    ] {
        assert!(body.contains(&expected), "{expected} missing from {body}");
    }
}

#[tokio::test]
async fn app_files_are_served_and_dotdot_is_refused() {
    let mut fixture = Fixture::start().await;
    fixture.state.web_dir = Some(bundle(&fixture));

    let hashed = fixture
        .get("/ui/app/cocat-web-0123456789abcdef.js", None)
        .await;
    let plain = fixture.get("/ui/app/mark.png", None).await;

    assert_eq!(hashed.status(), StatusCode::OK);
    assert!(header_of(&hashed, header::CONTENT_TYPE).contains("javascript"));
    assert_eq!(
        header_of(&hashed, header::CACHE_CONTROL),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(body_text(hashed).await, "export {}");
    assert_eq!(plain.status(), StatusCode::OK);
    assert_eq!(header_of(&plain, header::CACHE_CONTROL), "no-cache");
    for refused in [
        "/ui/app/../secret.txt",
        "/ui/app/%2e%2e/secret.txt",
        "/ui/app/snippets/",
        "/ui/app/snippets",
        "/ui/app/missing.js",
    ] {
        let response = fixture.get(refused, None).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{refused}");
        assert!(!body_text(response).await.contains("secret"), "{refused}");
    }
}

#[tokio::test]
async fn without_a_web_dir_browse_says_not_installed() {
    let mut fixture = Fixture::start().await;
    let (room, viewer) = room_with_viewer(&fixture).await;
    let token = fixture.token(viewer).await;
    let path = format!("/ui/rooms/{room}/browse");

    let unset = fixture.get(&path, Some(&token)).await;
    fixture.state.web_dir = Some(fixture.dir().join("absent"));
    let missing = fixture.get(&path, Some(&token)).await;

    for response in [unset, missing] {
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = body_text(response).await;
        assert!(
            body.contains("The catalogue browser is not installed on this server"),
            "{body}"
        );
        assert!(
            body.contains(&format!(r#"href="/ui/rooms/{room}""#)),
            "{body}"
        );
    }
    assert_eq!(
        fixture.get("/ui/app/mark.png", None).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn the_room_page_opens_the_catalogues() {
    let fixture = Fixture::start().await;
    let (room, viewer) = room_with_viewer(&fixture).await;
    let token = fixture.token(viewer).await;

    let page = body_text(
        fixture
            .get(&format!("/ui/rooms/{room}"), Some(&token))
            .await,
    )
    .await;

    assert!(
        page.contains(&format!(
            r#"href="/ui/rooms/{room}/browse">Open catalogues</a>"#
        )),
        "{page}"
    );
}
