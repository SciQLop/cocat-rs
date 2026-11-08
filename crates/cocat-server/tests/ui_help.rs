mod ui_support;

use axum::http::StatusCode;
use cocat_server::{groups, lifecycle};
use ui_support::{Fixture, PUBLIC_URL, body_text};

const MEMBER: &str = "member@example.org";
const ANCHORS: [&str; 10] = [
    "projects",
    "catalogues",
    "smart-catalogues",
    "history",
    "sharing",
    "groups",
    "roles",
    "presence",
    "sciqlop-python",
    "account",
];

async fn page(fixture: &Fixture, path: &str, token: Option<&str>) -> String {
    let response = fixture.get(path, token).await;
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    body_text(response).await
}

fn links_to(page: &str, anchor: &str) {
    let href = format!(r#"href="/ui/help#{anchor}""#);
    assert!(page.contains(&href), "the page links to {href}: {page}");
}

#[tokio::test]
async fn the_help_page_opens_without_signing_in() {
    let fixture = Fixture::start().await;

    let help = page(&fixture, "/ui/help", None).await;

    for anchor in ANCHORS {
        assert!(help.contains(&format!(r#"id="{anchor}""#)), "{anchor}");
        assert!(
            help.contains(&format!(r##"href="#{anchor}""##)),
            "contents: {anchor}"
        );
    }
    assert!(help.contains(r#"<a href="/ui/help">Help</a>"#), "{help}");
}

#[tokio::test]
async fn the_signed_in_navigation_has_help() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;

    let rooms = page(&fixture, "/ui/rooms", Some(&token)).await;

    assert!(rooms.contains(r#"<a href="/ui/help">Help</a>"#), "{rooms}");
}

#[tokio::test]
async fn the_python_example_names_the_public_address() {
    let public = Fixture::start_public().await;
    let local = Fixture::start().await;

    let configured = page(&public, "/ui/help", None).await;
    let unconfigured = page(&local, "/ui/help", None).await;

    assert!(
        configured.contains(&format!(r#"url = "{PUBLIC_URL}""#)),
        "{configured}"
    );
    assert!(
        unconfigured.contains(r#"url = "https://your-cocat-server""#),
        "{unconfigured}"
    );
}

#[tokio::test]
async fn project_pages_link_to_their_help() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;

    let empty = page(&fixture, "/ui/rooms", Some(&token)).await;
    let room = lifecycle::create_room(&fixture.state, user, "lab")
        .await
        .unwrap();
    let listed = page(&fixture, "/ui/rooms", Some(&token)).await;
    let project = page(&fixture, &format!("/ui/rooms/{}", room.id), Some(&token)).await;

    links_to(&empty, "projects");
    links_to(&listed, "projects");
    for anchor in ["catalogues", "roles", "groups", "sharing"] {
        links_to(&project, anchor);
    }
    assert!(project.contains(r#"aria-label="Help: roles""#), "{project}");
}

#[tokio::test]
async fn group_pages_link_to_their_help() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;

    let empty = page(&fixture, "/ui/groups", Some(&token)).await;
    let group = groups::create(&fixture.state, user, "team").await.unwrap();
    let listed = page(&fixture, "/ui/groups", Some(&token)).await;
    let detail = page(&fixture, &format!("/ui/groups/{}", group.id), Some(&token)).await;

    for page in [empty, listed, detail] {
        links_to(&page, "groups");
    }
}

#[tokio::test]
async fn joining_and_account_pages_link_to_their_help() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;

    links_to(&page(&fixture, "/ui/invitation", None).await, "sharing");
    links_to(&page(&fixture, "/ui/groups/join", None).await, "groups");
    links_to(
        &page(&fixture, "/ui/account", Some(&token)).await,
        "account",
    );
}

/// The `<section>` that holds the anchor, as it reads once the tags and line breaks are gone.
fn section_text(page: &str, anchor: &str) -> String {
    let start = page.find(&format!(r#"id="{anchor}""#)).unwrap();
    let end = start + page[start..].find("</section>").unwrap();
    let mut text = String::new();
    let mut in_tag = false;
    for character in page[start..end].chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(character),
            _ => {}
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[tokio::test]
async fn the_history_help_explains_copies_in_plain_words() {
    let fixture = Fixture::start().await;

    let help = page(&fixture, "/ui/help", None).await;

    let history = section_text(&help, "history");
    assert!(
        history.contains("Make a copy from this version"),
        "{history}"
    );
    for word in ["fork", "branch", "merge", "commit", "diff"] {
        assert!(!history.to_lowercase().contains(word), "{word}: {history}");
    }
}

#[tokio::test]
async fn the_smart_catalogue_help_names_the_freeze_button() {
    let fixture = Fixture::start().await;

    let help = page(&fixture, "/ui/help", None).await;

    let smart = section_text(&help, "smart-catalogues");
    assert!(smart.contains("More → Freeze…"), "{smart}");
    assert!(!smart.to_lowercase().contains("materializ"), "{smart}");
}
