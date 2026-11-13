//! The banner an administrator sets on the overview page, shown above every page.

mod ui_support;

use axum::http::StatusCode;
use cocat_server::links::Links;
use cocat_server::{admin, db, lifecycle};
use ui_support::{Fixture, body_text, location};

const FORM: &str = "text=cocat-dev+moves+to+%2Fcocat+on+%3CMonday%3E%3A+no+data+will+be+lost.";
const SHOWN: &str = "cocat-dev moves to /cocat on &#60;Monday&#62;: no data will be lost.";

async fn admin_token(fixture: &Fixture) -> String {
    let id = db::create_user(&fixture.state.pool, "admin@example.org", "pwd", true, true)
        .await
        .unwrap();
    fixture.token(id).await
}

#[tokio::test]
async fn an_administrator_sets_the_notice_every_server_page_shows() {
    let fixture = Fixture::start().await;
    let token = admin_token(&fixture).await;

    let response = fixture.post("/ui/admin/notice", Some(&token), FORM).await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/ui/admin");
    let body = body_text(fixture.get("/", None).await).await;
    let banner = body.find("class=\"server-notice\"").expect("the banner");
    assert!(banner < body.find("site-header").unwrap());
    assert!(body.contains(SHOWN), "escaped");
    assert!(!body.contains("<Monday>"));
    let overview = body_text(fixture.get("/ui/admin", Some(&token)).await).await;
    assert!(
        overview.contains("value=\"cocat-dev moves"),
        "the form shows it"
    );
}

#[tokio::test]
async fn the_catalogue_app_shows_the_notice_first_in_its_body() {
    let fixture = Fixture::start().await.with_app();
    let token = admin_token(&fixture).await;
    fixture.post("/ui/admin/notice", Some(&token), FORM).await;
    let user = fixture.verified("owner@example.org").await;
    let room = lifecycle::create_room(&fixture.state, user, "A project")
        .await
        .unwrap();
    let member = fixture.token(user).await;

    let response = fixture
        .get(&format!("/ui/rooms/{}/browse", room.id), Some(&member))
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    let after_body = body.split_once("<body>").expect("a body").1;
    assert!(
        after_body
            .trim_start()
            .starts_with("<p class=\"server-notice\""),
        "{after_body}"
    );
    assert!(body.contains(SHOWN));
}

#[tokio::test]
async fn an_empty_notice_removes_the_banner() {
    let fixture = Fixture::start().await;
    let token = admin_token(&fixture).await;
    fixture.post("/ui/admin/notice", Some(&token), FORM).await;

    fixture
        .post("/ui/admin/notice", Some(&token), "text=+++")
        .await;

    let body = body_text(fixture.get("/", None).await).await;
    assert!(!body.contains("server-notice"));
    assert_eq!(db::notice(&fixture.state.pool).await.unwrap(), None);
}

#[tokio::test]
async fn a_member_cannot_set_the_notice() {
    let fixture = Fixture::start().await;
    let member = fixture.verified("member@example.org").await;
    let token = fixture.token(member).await;

    let response = fixture.post("/ui/admin/notice", Some(&token), FORM).await;

    assert_eq!(location(&response), "/ui/rooms");
    let body = body_text(fixture.get("/", None).await).await;
    assert!(!body.contains("server-notice"));
}

#[tokio::test]
async fn the_notice_outlives_a_restart() {
    let fixture = Fixture::start().await;
    let token = admin_token(&fixture).await;
    fixture.post("/ui/admin/notice", Some(&token), FORM).await;

    let restarted = Links::from_env(&|_| None).unwrap();
    admin::restore_notice(&fixture.state.pool, &restarted)
        .await
        .unwrap();

    assert!(
        restarted
            .notice()
            .unwrap()
            .ends_with("no data will be lost.")
    );
}

#[tokio::test]
async fn a_notice_longer_than_the_limit_is_refused() {
    let fixture = Fixture::start().await;
    let token = admin_token(&fixture).await;
    let long = format!("text={}", "a".repeat(admin::MAX_NOTICE_CHARS + 1));

    let response = fixture.post("/ui/admin/notice", Some(&token), &long).await;

    assert_eq!(location(&response), "/ui/admin");
    assert_eq!(db::notice(&fixture.state.pool).await.unwrap(), None);
    assert_eq!(fixture.state.links.notice(), None);
}
