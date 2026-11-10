mod ui_support;

use std::fs;
use std::path::PathBuf;

use axum::http::{StatusCode, header};
use axum::response::Response;
use cocat_core::role::Role;
use cocat_server::links::Links;
use cocat_server::{db, lifecycle};
use ui_support::{Fixture, body_text, flash_cleared, location};
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
    assert!(body_text(response).await.contains("Project not found"));
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
    assert!(
        !body.contains("cocat-admin"),
        "only an admin's page offers the admin pages"
    );
    let base = body.find("<base").unwrap();
    assert!(
        base < body.find("<title>").unwrap(),
        "the base must come before anything that loads"
    );
}

#[tokio::test]
async fn an_admins_page_says_so() {
    let mut fixture = Fixture::start().await;
    fixture.state.web_dir = Some(bundle(&fixture));
    let admin = db::create_user(&fixture.state.pool, "admin@example.org", "p", true, true)
        .await
        .unwrap();
    let room = lifecycle::create_room(&fixture.state, admin, "Lab")
        .await
        .unwrap()
        .id;
    let token = fixture.token(admin).await;

    let body = body_text(
        fixture
            .get(&format!("/ui/rooms/{room}/browse"), Some(&token))
            .await,
    )
    .await;

    assert!(
        body.contains(r#"<meta name="cocat-admin" content="true">"#),
        "{body}"
    );
}

#[tokio::test]
async fn the_app_page_says_when_a_project_has_files() {
    let mut fixture = Fixture::start().await;
    fixture.state.web_dir = Some(bundle(&fixture));
    let owner = db::create_user(&fixture.state.pool, "owner@example.org", "p", true, false)
        .await
        .unwrap();
    let workspace = lifecycle::create_project(
        &fixture.state,
        owner,
        "Notebooks",
        db::ProjectKind::Workspace,
    )
    .await
    .unwrap()
    .id;
    let catalogues = lifecycle::create_room(&fixture.state, owner, "Lab")
        .await
        .unwrap()
        .id;
    let token = fixture.token(owner).await;
    let page = |room: String| {
        let (fixture, token) = (&fixture, &token);
        async move {
            body_text(
                fixture
                    .get(&format!("/ui/rooms/{room}/browse"), Some(token))
                    .await,
            )
            .await
        }
    };

    let body = page(workspace).await;
    assert!(
        body.contains(r#"<meta name="cocat-files" content="true">"#),
        "{body}"
    );
    assert!(!page(catalogues).await.contains("cocat-files"));
}

#[tokio::test]
async fn the_app_page_names_the_largest_file_a_project_takes() {
    let mut fixture = Fixture::start().await;
    fixture.state.web_dir = Some(bundle(&fixture));
    let owner = db::create_user(&fixture.state.pool, "owner@example.org", "p", true, false)
        .await
        .unwrap();
    let lab = lifecycle::create_room(&fixture.state, owner, "Lab")
        .await
        .unwrap()
        .id;
    let token = fixture.token(owner).await;

    let body = body_text(
        fixture
            .get(&format!("/ui/rooms/{lab}/browse"), Some(&token))
            .await,
    )
    .await;

    let cap = fixture.state.rooms.limits().file;
    assert!(
        body.contains(&format!(r#"<meta name="cocat-file-max" content="{cap}">"#)),
        "{body}"
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
    }
    assert_eq!(
        fixture.get("/ui/app/mark.png", None).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn the_project_page_passes_its_notice_into_the_app() {
    let mut fixture = Fixture::start().await;
    fixture.state.web_dir = Some(bundle(&fixture));
    let (room, viewer) = room_with_viewer(&fixture).await;
    let token = fixture.token(viewer).await;
    let notice = "cocat_flash=Removed%20b%40example.org";

    let moved = fixture
        .get_with(&format!("/ui/rooms/{room}"), &token, notice)
        .await;

    assert_eq!(moved.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&moved), format!("/ui/rooms/{room}/browse#share"));
    assert!(
        !flash_cleared(&moved),
        "a redirect shows nothing, so it keeps the notice"
    );
    let app = fixture
        .get_with(&format!("/ui/rooms/{room}/browse"), &token, notice)
        .await;
    assert!(flash_cleared(&app), "the app showed it");
    let body = body_text(app).await;
    assert!(
        body.contains(r#"<meta name="cocat-flash" content="Removed b@example.org">"#),
        "{body}"
    );
    assert!(!body.contains("cocat-share-link"), "{body}");
}

#[tokio::test]
async fn the_app_requires_login() {
    let fixture = Fixture::start().await;

    let response = fixture.get("/ui/app", None).await;

    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(location(&response), "/ui/login?next=%2Fui%2Fapp");
}

#[tokio::test]
async fn the_app_opens_the_personal_project() {
    let fixture = Fixture::start().await.with_app();
    let user = fixture.verified(OWNER).await;
    lifecycle::create_room(&fixture.state, user, "A first project")
        .await
        .unwrap();
    db::create_personal_room(&fixture.state.pool, user)
        .await
        .unwrap();
    let personal = db::personal_room(&fixture.state.pool, user)
        .await
        .unwrap()
        .unwrap();
    let token = fixture.token(user).await;

    let response = fixture.get("/ui/app", Some(&token)).await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), format!("/ui/rooms/{personal}/browse"));
}

/// The own project is a mark on the account, not a name: renamed, it still opens, and a
/// project merely called "Personal" does not.
#[tokio::test]
async fn the_app_opens_the_own_project_after_a_rename() {
    let fixture = Fixture::start().await.with_app();
    let user = fixture.verified(OWNER).await;
    lifecycle::create_room(&fixture.state, user, "Personal")
        .await
        .unwrap();
    db::create_personal_room(&fixture.state.pool, user)
        .await
        .unwrap();
    let own = db::personal_room(&fixture.state.pool, user)
        .await
        .unwrap()
        .unwrap();
    lifecycle::rename_room(&fixture.state, &own, user, "Mine")
        .await
        .unwrap();
    let token = fixture.token(user).await;

    let response = fixture.get("/ui/app", Some(&token)).await;

    assert_eq!(location(&response), format!("/ui/rooms/{own}/browse"));
}

#[tokio::test]
async fn someone_else_s_own_project_is_not_the_one_opened() {
    let fixture = Fixture::start().await.with_app();
    let owner = fixture.verified(OWNER).await;
    let viewer = fixture.verified(VIEWER).await;
    db::create_personal_room(&fixture.state.pool, owner)
        .await
        .unwrap();
    let theirs = db::personal_room(&fixture.state.pool, owner)
        .await
        .unwrap()
        .unwrap();
    db::add_member(&fixture.state.pool, &theirs, viewer, Role::Viewer)
        .await
        .unwrap();
    db::create_personal_room(&fixture.state.pool, viewer)
        .await
        .unwrap();
    let own = db::personal_room(&fixture.state.pool, viewer)
        .await
        .unwrap()
        .unwrap();
    let token = fixture.token(viewer).await;

    let response = fixture.get("/ui/app", Some(&token)).await;

    assert_eq!(location(&response), format!("/ui/rooms/{own}/browse"));
}

#[tokio::test]
async fn with_its_own_project_deleted_the_app_opens_the_first_one() {
    let fixture = Fixture::start().await.with_app();
    let (room, viewer) = room_with_viewer(&fixture).await;
    db::create_personal_room(&fixture.state.pool, viewer)
        .await
        .unwrap();
    let own = db::personal_room(&fixture.state.pool, viewer)
        .await
        .unwrap()
        .unwrap();
    lifecycle::delete_room(&fixture.state, &own, viewer)
        .await
        .unwrap();
    let token = fixture.token(viewer).await;

    let response = fixture.get("/ui/app", Some(&token)).await;

    assert_eq!(location(&response), format!("/ui/rooms/{room}/browse"));
}

#[tokio::test]
async fn without_a_personal_project_the_app_opens_the_first_one() {
    let fixture = Fixture::start().await.with_app();
    let (room, viewer) = room_with_viewer(&fixture).await;
    let token = fixture.token(viewer).await;

    let response = fixture.get("/ui/app", Some(&token)).await;

    assert_eq!(location(&response), format!("/ui/rooms/{room}/browse"));
}

#[tokio::test]
async fn without_any_project_the_app_sends_to_the_projects_list() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(STRANGER).await;
    let token = fixture.token(user).await;

    let response = fixture.get("/ui/app", Some(&token)).await;

    assert_eq!(location(&response), "/ui/rooms");
}

/// Ids imported from Python can hold anything; every link and redirect escapes them as one
/// path segment, and the routes read them back whole.
#[tokio::test]
async fn a_project_id_with_url_characters_survives_every_link() {
    let mut fixture = Fixture::start().await;
    fixture.state.web_dir = Some(bundle(&fixture));
    let owner = fixture.verified(OWNER).await;
    let id = "Bepi MSA?#%";
    let path = "Bepi%20MSA%3F%23%25";
    imported_room(&fixture, owner, id).await;
    let token = fixture.token(owner).await;

    let app = fixture.get("/ui/app", Some(&token)).await;
    let page = fixture
        .get(&format!("/ui/rooms/{path}"), Some(&token))
        .await;
    let invite = fixture
        .post(
            &format!("/ui/rooms/{path}/invitations"),
            Some(&token),
            "role=viewer&kind=link",
        )
        .await;
    let browse = fixture
        .get(&format!("/ui/rooms/{path}/browse"), Some(&token))
        .await;

    assert_eq!(location(&app), format!("/ui/rooms/{path}/browse"));
    assert_eq!(location(&page), format!("/ui/rooms/{path}/browse#share"));
    assert_eq!(location(&invite), format!("/ui/rooms/{path}"));
    assert_eq!(browse.status(), StatusCode::OK);
    let body = body_text(browse).await;
    assert!(
        body.contains(&format!(r#"<meta name="cocat-ws" content="/room/{path}">"#)),
        "{body}"
    );
    assert!(
        body.contains(r#"<meta name="cocat-room" content="Bepi MSA?#%">"#),
        "{body}"
    );
}

/// What the import writes for a Python room: the id as it was, unchecked.
async fn imported_room(fixture: &Fixture, owner: Uuid, id: &str) {
    sqlx::query("INSERT INTO room (id, name, created_by) VALUES (?, 'Bepi MSA', ?)")
        .bind(id)
        .bind(owner.to_string())
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    db::add_member(&fixture.state.pool, id, owner, Role::Owner)
        .await
        .unwrap();
}

/// Without the bundle the app cannot open, so the server's own pages manage projects.
#[tokio::test]
async fn without_the_bundle_the_projects_and_groups_pages_render() {
    for web_dir in [None, Some("absent")] {
        let mut fixture = Fixture::start().await;
        fixture.state.web_dir = web_dir.map(|name| fixture.dir().join(name));
        let (room, viewer) = room_with_viewer(&fixture).await;
        let token = fixture.token(viewer).await;

        let entry = fixture.get("/ui/app", Some(&token)).await;
        let projects = fixture.get("/ui/rooms", Some(&token)).await;
        let groups = fixture.get("/ui/groups", Some(&token)).await;
        let project = fixture
            .get(&format!("/ui/rooms/{room}"), Some(&token))
            .await;

        assert_eq!(location(&entry), "/ui/rooms", "{web_dir:?}");
        assert_eq!(projects.status(), StatusCode::OK, "{web_dir:?}");
        assert_eq!(groups.status(), StatusCode::OK, "{web_dir:?}");
        assert_eq!(project.status(), StatusCode::OK, "{web_dir:?}");
        let page = body_text(project).await;
        assert!(page.contains("Your role: viewer"), "{page}");
        assert!(page.contains(OWNER), "{page}");
    }
}

/// A public catalogue opens for any account, as a viewer, and as an editor for a curator.
#[tokio::test]
async fn browse_opens_a_public_catalogue_for_everyone_signed_in() {
    let mut fixture = Fixture::start().await;
    fixture.state.web_dir = Some(bundle(&fixture));
    let reader = fixture.verified(STRANGER).await;
    let curator = fixture.verified(OWNER).await;
    db::set_curator(&fixture.state.pool, curator, true)
        .await
        .unwrap();
    let room = db::PublicRoom {
        id: "public.amda.shocks".to_owned(),
        name: "Shocks".to_owned(),
        folder: None,
        source: None,
        description: None,
        public_count: None,
        counted_file: None,
    };
    assert!(
        db::create_public_room(&fixture.state.pool, &room, curator)
            .await
            .unwrap()
    );

    for (user, role) in [(reader, "viewer"), (curator, "editor")] {
        let token = fixture.token(user).await;
        let response = fixture
            .get("/ui/rooms/public.amda.shocks/browse", Some(&token))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        let expected = format!(r#"<meta name="cocat-role" content="{role}">"#);
        assert!(body.contains(&expected), "{expected} missing from {body}");
        assert!(body.contains(r#"<meta name="cocat-room-name" content="Shocks">"#));
    }
}
