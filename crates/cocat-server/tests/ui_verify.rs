mod ui_support;

use std::time::SystemTime;

use axum::http::StatusCode;
use cocat_server::{accounts, db};
use ui_support::{Fixture, PASSWORD, body_text, form, location};

const NEW: &str = "verify@example.org";

fn token(fixture: &Fixture, id: uuid::Uuid, email: &str) -> String {
    accounts::verification_token(id, email, &fixture.state.secret, SystemTime::now()).unwrap()
}

#[tokio::test]
async fn the_page_explains_without_a_token() {
    let fixture = Fixture::start().await;

    let page = body_text(fixture.get("/ui/verify", None).await).await;

    assert!(
        page.contains("Open the link from your inbox to verify your address."),
        "{page}"
    );
    assert!(
        page.contains(r#"<form method="post" action="/ui/verify""#),
        "{page}"
    );
    assert!(page.contains(r#"type="hidden" name="token""#), "{page}");
    assert!(page.contains(">Verify<"), "{page}");
    assert!(page.contains(r#"href="/ui/rooms""#), "{page}");
}

#[tokio::test]
async fn the_page_posts_the_fragment_on_load() {
    let fixture = Fixture::start().await;

    let page = body_text(fixture.get("/ui/verify", None).await).await;

    assert!(page.contains("/ui/static/fragment.js"), "{page}");
    assert!(page.contains("data-submit-fragment"), "{page}");
}

#[tokio::test]
async fn a_valid_token_verifies_the_account() {
    let fixture = Fixture::start().await;
    let id = fixture.unverified(NEW).await;

    let response = fixture
        .post(
            "/ui/verify",
            None,
            &form(&[("token", &token(&fixture, id, NEW))]),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let page = body_text(response).await;
    assert!(page.contains("Your address is verified."), "{page}");
    assert!(page.contains(r#"href="/ui/rooms""#), "{page}");
    assert!(
        !page.contains("data-submit-fragment"),
        "the answered page posted again: {page}"
    );
    let user = db::user_by_email(&fixture.state.pool, NEW)
        .await
        .unwrap()
        .expect("the account exists");
    assert!(user.is_verified);
    let signed_in = fixture.login(NEW, PASSWORD, None).await;
    assert_eq!(location(&signed_in), "/ui/rooms");
}

#[tokio::test]
async fn an_already_verified_address_is_reported() {
    let fixture = Fixture::start().await;
    let id = fixture.unverified(NEW).await;
    let token = token(&fixture, id, NEW);
    fixture
        .post("/ui/verify", None, &form(&[("token", &token)]))
        .await;

    let response = fixture
        .post("/ui/verify", None, &form(&[("token", &token)]))
        .await;

    let page = body_text(response).await;
    assert!(page.contains("Your address is already verified."), "{page}");
}

#[tokio::test]
async fn a_bad_token_is_reported() {
    let fixture = Fixture::start().await;

    let response = fixture
        .post("/ui/verify", None, &form(&[("token", "not-a-token")]))
        .await;

    let page = body_text(response).await;
    assert!(
        page.contains("This verification link is not valid anymore."),
        "{page}"
    );
}

#[tokio::test]
async fn without_a_token_the_page_explains_what_to_do() {
    let fixture = Fixture::start().await;

    let response = fixture
        .post("/ui/verify", None, &form(&[("token", "")]))
        .await;

    let page = body_text(response).await;
    assert!(
        page.contains("Open the link from your inbox to verify your address."),
        "{page}"
    );
}

#[tokio::test]
async fn the_token_never_reaches_the_page_again() {
    let fixture = Fixture::start().await;
    let id = fixture.unverified(NEW).await;
    let token = token(&fixture, id, NEW);

    let response = fixture
        .post("/ui/verify", None, &form(&[("token", &token)]))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let page = body_text(response).await;

    assert!(!page.contains(&token), "{page}");
}
