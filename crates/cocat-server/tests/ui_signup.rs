mod ui_support;

use axum::http::StatusCode;
use cocat_server::db;
use ui_support::{Fixture, PASSWORD, body_text, cookie_token, location, set_cookie};

const NEW: &str = "signup@example.org";
const PLACEHOLDER: &str = "orcid-77@orcid.placeholder.cocat.org";

#[tokio::test]
async fn the_page_renders_the_form() {
    let fixture = Fixture::start().await;

    let page = body_text(fixture.get("/ui/signup", None).await).await;

    assert!(
        page.contains(r#"<form method="post" action="/ui/signup">"#),
        "{page}"
    );
    assert!(page.contains(r#"name="email""#), "{page}");
    assert!(page.contains(r#"name="password""#), "{page}");
    assert!(page.contains(r#"type="password""#), "{page}");
    assert!(page.contains("Sign up"), "{page}");
    assert!(page.contains(r#"href="/ui/login""#), "{page}");
}

#[tokio::test]
async fn sign_up_with_a_mailer_asks_for_verification() {
    let fixture = Fixture::start_with_mail().await;

    let response = fixture.sign_up(NEW, PASSWORD).await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/ui/login?created=1");
    assert!(set_cookie(&response).is_none(), "a cookie was set");
    let mail = fixture.wait_for_mail(NEW).await;
    assert!(mail.contains("/ui/verify#"), "{mail}");
    let user = db::user_by_email(&fixture.state.pool, NEW)
        .await
        .unwrap()
        .expect("the account exists");
    assert!(!user.is_verified);
}

#[tokio::test]
async fn the_created_message_reaches_the_login_page() {
    let fixture = Fixture::start_with_mail().await;
    fixture.sign_up(NEW, PASSWORD).await;

    let page = body_text(fixture.get("/ui/login?created=1", None).await).await;

    assert!(
        page.contains("Account created. Check your inbox to verify it."),
        "{page}"
    );
}

#[tokio::test]
async fn sign_up_without_a_mailer_lands_in_the_rooms() {
    let fixture = Fixture::start().await;

    let response = fixture.sign_up(NEW, PASSWORD).await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/ui/rooms");
    let cookie = set_cookie(&response).expect("a session cookie");
    let token = cookie_token(&cookie);
    let rooms = fixture.get("/ui/rooms", Some(&token)).await;
    assert_eq!(rooms.status(), StatusCode::OK);
}

#[tokio::test]
async fn taken_address_with_mail_looks_like_success() {
    let fixture = Fixture::start_with_mail().await;
    fixture.verified(NEW).await;

    let response = fixture.sign_up(NEW, PASSWORD).await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/ui/login?created=1");
    let mail = fixture.wait_for_mail(NEW).await;
    assert!(
        mail.contains("Subject: A sign-up used your cocat address"),
        "{mail}"
    );
}

#[tokio::test]
async fn taken_address_without_mail_says_exists() {
    let fixture = Fixture::start().await;
    fixture.verified(NEW).await;

    let response = fixture.sign_up(NEW, PASSWORD).await;

    assert_eq!(location(&response), "/ui/signup?error=exists");
}

#[tokio::test]
async fn an_address_that_is_not_an_email_is_refused() {
    let fixture = Fixture::start().await;

    let response = fixture.sign_up("not-an-address", PASSWORD).await;

    assert_eq!(location(&response), "/ui/signup?error=email");
    assert!(
        db::user_by_email(&fixture.state.pool, "not-an-address")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_placeholder_address_cannot_be_registered() {
    let fixture = Fixture::start().await;

    let response = fixture.sign_up(PLACEHOLDER, PASSWORD).await;

    assert_eq!(location(&response), "/ui/signup?error=email");
    assert!(
        db::user_by_email(&fixture.state.pool, PLACEHOLDER)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_mailer_that_fails_still_creates_the_account() {
    let fixture = Fixture::start_with_mail().await;
    fixture.break_mail();

    let response = fixture.sign_up(NEW, PASSWORD).await;

    assert_eq!(location(&response), "/ui/login?created=1");
    assert!(
        db::user_by_email(&fixture.state.pool, NEW)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_flooded_limiter_is_refused() {
    let fixture = Fixture::start().await;
    for _ in 0..10 {
        fixture.sign_up("not-an-address", PASSWORD).await;
    }

    let response = fixture.sign_up(NEW, PASSWORD).await;

    assert_eq!(location(&response), "/ui/signup?error=too_many");
}

#[tokio::test]
async fn the_signup_page_shows_the_message() {
    let fixture = Fixture::start().await;

    let page = body_text(fixture.get("/ui/signup?error=exists", None).await).await;

    assert!(page.contains("That email is already registered."), "{page}");
}
