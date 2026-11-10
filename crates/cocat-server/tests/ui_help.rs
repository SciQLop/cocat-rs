mod ui_support;

use axum::http::StatusCode;
use cocat_server::groups;
use ui_support::{Fixture, PUBLIC_URL, body_text};

const MEMBER: &str = "member@example.org";
const ANCHORS: [&str; 15] = [
    "projects",
    "workspaces",
    "catalogues",
    "statistics",
    "import-export",
    "searching",
    "smart-catalogues",
    "history",
    "sharing",
    "groups",
    "public",
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

    links_to(&empty, "projects");
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
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
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
async fn the_public_help_says_how_to_submit_and_what_curators_do() {
    let fixture = Fixture::start().await;

    let help = page(&fixture, "/ui/help", None).await;

    let public = section_text(&help, "public");
    for words in [
        "on the left, under the catalogues",
        "the folders on the left",
        "All",
        "pick it in the menu at the top",
        "Submit to Public",
        "as an update…",
        "Propose a change…",
        "Copy to a project…",
        "next to it in the list",
        "My submissions",
        "Submissions",
        "What changed",
        "Accept",
        "Decline",
        "Withdraw",
    ] {
        assert!(public.contains(words), "{words}: {public}");
    }
    for word in ["snapshot", "merge", "commit", "diff", "soon"] {
        assert!(!public.to_lowercase().contains(word), "{word}: {public}");
    }
}

#[tokio::test]
async fn only_the_server_side_names_still_call_a_project_a_room() {
    let fixture = Fixture::start().await;

    let help = page(&fixture, "/ui/help", None).await;

    let projects = section_text(&help, "projects");
    assert!(!projects.contains("SciQLop calls"), "{projects}");
    assert!(
        projects.contains("the server API, the websocket path and the command-line tool"),
        "{projects}"
    );
}

#[tokio::test]
async fn the_smart_catalogue_help_names_the_freeze_button() {
    let fixture = Fixture::start().await;

    let help = page(&fixture, "/ui/help", None).await;

    let smart = section_text(&help, "smart-catalogues");
    assert!(smart.contains("More → Freeze…"), "{smart}");
    assert!(!smart.to_lowercase().contains("materializ"), "{smart}");
}

#[tokio::test]
async fn the_smart_catalogue_help_says_how_to_make_one_in_the_browser() {
    let fixture = Fixture::start().await;

    let help = page(&fixture, "/ui/help", None).await;

    let smart = section_text(&help, "smart-catalogues");
    assert!(smart.contains("More → Add a rule…"), "{smart}");
}

#[tokio::test]
async fn the_statistics_help_says_what_coverage_and_gaps_mean() {
    let fixture = Fixture::start().await;

    let help = page(&fixture, "/ui/help", None).await;

    let statistics = section_text(&help, "statistics");
    assert!(statistics.contains("Coverage"), "{statistics}");
    assert!(statistics.contains("once"), "{statistics}");
    assert!(statistics.contains("Gaps"), "{statistics}");
}

#[tokio::test]
async fn the_import_help_says_which_files_and_what_updates() {
    let fixture = Fixture::start().await;

    let help = page(&fixture, "/ui/help", None).await;

    let section = section_text(&help, "import-export");
    let words = [
        "VOTable",
        "tscat JSON",
        "TFCat",
        "one file per catalogue",
        "Event shapes and their axis travel in VOTable, tscat JSON and TFCat",
        "CSV",
        "same id",
        "one change",
    ];
    for words in words {
        assert!(section.contains(words), "{words}: {section}");
    }
}

#[tokio::test]
async fn the_workspace_help_says_what_the_web_app_shows() {
    let fixture = Fixture::start().await;
    let help = page(&fixture, "/ui/help", None).await;
    let workspaces = section_text(&help, "workspaces");
    for expected in [
        "SciQLop",
        "File history",
        "Restore",
        "New SciQLop",
        "download",
        "live",
    ] {
        assert!(
            workspaces.contains(expected),
            "{expected} missing: {workspaces}"
        );
    }
}

#[tokio::test]
async fn the_search_help_gives_the_syntax_and_how_to_save_one() {
    let fixture = Fixture::start().await;

    let help = page(&fixture, "/ui/help", None).await;

    let searching = section_text(&help, "searching");
    for expected in [
        "tag:shock",
        "confidence:>=0.8",
        "created:now-7d",
        "_exists_:shape",
        "Save as new smart catalogue…",
        "This catalogue",
        "Save to",
        "Build a search",
        "not on a timer",
        "All events",
        "suggests as you type",
    ] {
        assert!(
            searching.contains(expected),
            "{expected} missing: {searching}"
        );
    }
}

#[tokio::test]
async fn the_catalogue_help_says_what_a_link_is() {
    let fixture = Fixture::start().await;

    let help = page(&fixture, "/ui/help", None).await;

    let catalogues = section_text(&help, "catalogues");
    assert!(
        catalogues.contains("Link a catalogue from another project…"),
        "{catalogues}"
    );
    assert!(
        catalogues.contains("changes go to the original"),
        "{catalogues}"
    );
    assert!(catalogues.contains("your role there"), "{catalogues}");
    assert!(catalogues.contains("No longer available"), "{catalogues}");
    assert!(
        catalogues.contains("A smart catalogue cannot use a linked catalogue"),
        "{catalogues}"
    );
}
