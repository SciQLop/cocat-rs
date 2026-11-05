mod ui_support;

use std::time::{Duration, Instant, SystemTime};

use axum::http::StatusCode;
use cocat_server::{accounts, db};
use ui_support::{
    Fixture, PASSWORD, PUBLIC_URL, body_text, form, link_token, location, set_cookie,
};

const MEMBER: &str = "member@example.org";
const NEW_PASSWORD: &str = "brand-new-password";
const FORGOT: &str = "/ui/forgot-password";

#[tokio::test]
async fn the_forgot_page_renders_the_form() {
    let fixture = Fixture::start().await;

    let page = body_text(fixture.get(FORGOT, None).await).await;

    assert!(
        page.contains(r#"<form method="post" action="/ui/forgot-password">"#),
        "{page}"
    );
    assert!(page.contains(r#"name="email""#), "{page}");
    assert!(page.contains(">Send a reset link<"), "{page}");
}

#[tokio::test]
async fn an_unknown_address_is_told_nothing() {
    let fixture = Fixture::start_with_mail().await;

    let response = fixture.forgot("nobody@example.org").await;

    assert_eq!(location(&response), "/ui/forgot-password?sent=1");
    assert!(fixture.mails().is_empty());
}

#[tokio::test]
async fn a_known_address_gets_a_link() {
    let fixture = Fixture::start_with_mail().await;
    fixture.verified(MEMBER).await;

    let response = fixture.forgot(MEMBER).await;

    assert_eq!(location(&response), "/ui/forgot-password?sent=1");
    let mail = fixture.wait_for_mail(MEMBER).await;
    assert!(mail.contains("/ui/reset-password#"), "{mail}");
}

#[tokio::test]
async fn a_mailer_that_fails_is_swallowed() {
    let fixture = Fixture::start_with_mail().await;
    fixture.verified(MEMBER).await;
    fixture.break_mail();

    let response = fixture.forgot(MEMBER).await;

    assert_eq!(location(&response), "/ui/forgot-password?sent=1");
}

#[tokio::test]
async fn forgot_mail_is_not_awaited() {
    let fixture = Fixture::start_with_slow_mail().await;
    fixture.verified(MEMBER).await;

    let started = Instant::now();
    let response = fixture.forgot(MEMBER).await;
    let waited = started.elapsed();

    assert_eq!(location(&response), "/ui/forgot-password?sent=1");
    assert!(
        waited < Duration::from_millis(500),
        "the answer waited for the mail: {waited:?}"
    );
}

#[tokio::test]
async fn a_flooded_limiter_is_refused() {
    let fixture = Fixture::start().await;
    for _ in 0..10 {
        fixture.forgot("x@example.org").await;
    }

    let response = fixture.forgot(MEMBER).await;

    assert_eq!(location(&response), "/ui/forgot-password?error=too_many");
}

#[tokio::test]
async fn reset_token_travels_in_the_body() {
    let fixture = Fixture::start().await;

    let page = body_text(fixture.get("/ui/reset-password", None).await).await;

    assert!(
        page.contains(r#"<form method="post" action="/ui/reset-password">"#),
        "{page}"
    );
    assert!(page.contains(r#"type="hidden" name="token""#), "{page}");
    assert!(page.contains(r#"name="link""#), "{page}");
    assert!(page.contains("Reset link"), "{page}");
    assert!(!page.contains("reset-password?token"), "{page}");
}

#[tokio::test]
async fn the_whole_flow() {
    let fixture = Fixture::start_with_mail().await;
    fixture.verified(MEMBER).await;
    fixture.forgot(MEMBER).await;
    let mail = fixture.wait_for_mail(MEMBER).await;
    let token = link_token(&mail, "/ui/reset-password");

    let response = fixture
        .post(
            "/ui/reset-password",
            None,
            &form(&[
                ("token", &token),
                ("password", NEW_PASSWORD),
                ("repeat", NEW_PASSWORD),
            ]),
        )
        .await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/ui/login?reset=1");
    let signed_in = fixture.login(MEMBER, NEW_PASSWORD, None).await;
    assert_eq!(location(&signed_in), "/ui/rooms");
    assert!(set_cookie(&signed_in).is_some());
    let old = fixture.login(MEMBER, PASSWORD, None).await;
    assert_eq!(location(&old), "/ui/login?error=credentials");
}

#[tokio::test]
async fn a_pasted_link_works_without_scripts() {
    let fixture = Fixture::start_with_mail().await;
    fixture.verified(MEMBER).await;
    fixture.forgot(MEMBER).await;
    let mail = fixture.wait_for_mail(MEMBER).await;
    let token = link_token(&mail, "/ui/reset-password");
    let pasted = format!("{PUBLIC_URL}/ui/reset-password#{token}");

    let response = fixture
        .post(
            "/ui/reset-password",
            None,
            &form(&[
                ("token", ""),
                ("link", &pasted),
                ("password", NEW_PASSWORD),
                ("repeat", NEW_PASSWORD),
            ]),
        )
        .await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/ui/login?reset=1");
}

#[tokio::test]
async fn a_bad_token_is_reported() {
    let fixture = Fixture::start().await;

    let response = fixture
        .post(
            "/ui/reset-password",
            None,
            &form(&[
                ("token", "not-a-token"),
                ("password", NEW_PASSWORD),
                ("repeat", NEW_PASSWORD),
            ]),
        )
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let page = body_text(response).await;
    assert!(
        page.contains("This reset link is not valid anymore."),
        "{page}"
    );
}

#[tokio::test]
async fn an_empty_password_is_refused() {
    let fixture = Fixture::start_with_mail().await;
    fixture.verified(MEMBER).await;

    let response = fixture
        .post(
            "/ui/reset-password",
            None,
            &form(&[("token", "whatever"), ("password", ""), ("repeat", "")]),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let page = body_text(response).await;
    assert!(page.contains("Enter a new password"), "{page}");
    let old = fixture.login(MEMBER, PASSWORD, None).await;
    assert_eq!(old.status(), StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn a_mismatched_repeat_is_refused() {
    let fixture = Fixture::start_with_mail().await;
    fixture.verified(MEMBER).await;
    let user = db::user_by_email(&fixture.state.pool, MEMBER)
        .await
        .unwrap()
        .expect("the account exists");
    let token = accounts::reset_token(
        user.id,
        &user.hashed_password,
        &fixture.state.secret,
        SystemTime::now(),
    )
    .unwrap();

    let response = fixture
        .post(
            "/ui/reset-password",
            None,
            &form(&[
                ("token", &token),
                ("password", NEW_PASSWORD),
                ("repeat", "something-else"),
            ]),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let page = body_text(response).await;
    assert!(page.contains("The new passwords differ"), "{page}");
    assert!(!page.contains(&token), "the token was echoed: {page}");
    let old = fixture.login(MEMBER, PASSWORD, None).await;
    assert_eq!(location(&old), "/ui/rooms");
}
