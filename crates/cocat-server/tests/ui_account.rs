mod ui_support;

use axum::http::StatusCode;
use cocat_server::{accounts, db};
use ui_support::{Fixture, PASSWORD, body_text, form, location, set_cookie};

const MEMBER: &str = "member@example.org";
const NEW_PASSWORD: &str = "brand-new-password";
const PLACEHOLDER: &str = "orcid-1234@orcid.placeholder.cocat.org";

/// Move the member's address to a provider placeholder and hand back a session token.
async fn placeholder(fixture: &Fixture, email: &str) -> String {
    let id = match db::user_by_email(&fixture.state.pool, email).await.unwrap() {
        Some(user) => user.id,
        None => fixture.verified(email).await,
    };
    let user = db::user_by_id(&fixture.state.pool, id)
        .await
        .unwrap()
        .expect("the account exists");
    accounts::change(
        &fixture.state,
        user,
        accounts::Change {
            email: Some(PLACEHOLDER.to_owned()),
            ..accounts::Change::default()
        },
    )
    .await
    .unwrap();
    fixture.token(id).await
}

#[tokio::test]
async fn requires_login() {
    let fixture = Fixture::start().await;

    let response = fixture.get("/ui/account", None).await;

    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(location(&response), "/ui/login?next=%2Fui%2Faccount");
}

#[tokio::test]
async fn shows_the_signed_in_email() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;

    let page = body_text(fixture.get("/ui/account", Some(&token)).await).await;

    assert!(page.contains(MEMBER), "{page}");
    assert!(
        page.contains(r#"<form method="post" action="/ui/account/password">"#),
        "{page}"
    );
    assert!(page.contains(r#"name="current""#), "{page}");
    assert!(page.contains(r#"name="new""#), "{page}");
    assert!(page.contains(r#"name="repeat""#), "{page}");
    assert!(page.contains(">Change password<"), "{page}");
    assert!(!page.contains("/ui/confirm-email"), "{page}");
    assert!(!page.contains("signs in with"), "{page}");
}

#[tokio::test]
async fn a_placeholder_address_offers_a_confirmation_link() {
    let fixture = Fixture::start().await;
    let token = placeholder(&fixture, MEMBER).await;

    let page = body_text(fixture.get("/ui/account", Some(&token)).await).await;

    assert!(page.contains(PLACEHOLDER), "{page}");
    assert!(page.contains(r#"href="/ui/confirm-email""#), "{page}");
    assert!(page.contains("Confirm your email"), "{page}");
}

#[tokio::test]
async fn change_password() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;

    let response = fixture
        .post(
            "/ui/account/password",
            Some(&token),
            &form(&[
                ("current", PASSWORD),
                ("new", NEW_PASSWORD),
                ("repeat", NEW_PASSWORD),
            ]),
        )
        .await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/ui/login?error=password_changed");
    let cleared = set_cookie(&response).expect("the session cookie is cleared");
    assert!(cleared.contains("Max-Age=0"), "{cleared}");
    let ended = fixture.get("/ui/rooms", Some(&token)).await;
    assert_eq!(location(&ended), "/ui/login?next=%2Fui%2Frooms");
    let signed_in = fixture.login(MEMBER, NEW_PASSWORD, None).await;
    assert_eq!(location(&signed_in), "/ui/app");
    let old = fixture.login(MEMBER, PASSWORD, None).await;
    assert_eq!(location(&old), "/ui/login?error=credentials");
}

#[tokio::test]
async fn wrong_current_password_changes_nothing() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;

    let response = fixture
        .post(
            "/ui/account/password",
            Some(&token),
            &form(&[
                ("current", "not-my-password"),
                ("new", NEW_PASSWORD),
                ("repeat", NEW_PASSWORD),
            ]),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let page = body_text(response).await;
    assert!(page.contains("Wrong password"), "{page}");
    let old = fixture.login(MEMBER, PASSWORD, None).await;
    assert_eq!(location(&old), "/ui/app");
    let new = fixture.login(MEMBER, NEW_PASSWORD, None).await;
    assert_eq!(location(&new), "/ui/login?error=credentials");
}

#[tokio::test]
async fn a_mismatched_repeat_is_refused() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;

    let response = fixture
        .post(
            "/ui/account/password",
            Some(&token),
            &form(&[
                ("current", PASSWORD),
                ("new", NEW_PASSWORD),
                ("repeat", "other-password"),
            ]),
        )
        .await;

    let page = body_text(response).await;
    assert!(page.contains("The new passwords differ"), "{page}");
}

#[tokio::test]
async fn an_empty_new_password_is_refused() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;

    let response = fixture
        .post(
            "/ui/account/password",
            Some(&token),
            &form(&[("current", PASSWORD), ("new", ""), ("repeat", "")]),
        )
        .await;

    let page = body_text(response).await;
    assert!(page.contains("Enter a new password"), "{page}");
}

#[tokio::test]
async fn deactivated_cookie_is_anonymous() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;
    fixture.deactivate(MEMBER).await;

    let page = fixture.get("/ui/account", Some(&token)).await;
    assert_eq!(page.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(location(&page), "/ui/login?next=%2Fui%2Faccount");

    let response = fixture
        .post(
            "/ui/account/password",
            Some(&token),
            &form(&[
                ("current", PASSWORD),
                ("new", NEW_PASSWORD),
                ("repeat", NEW_PASSWORD),
            ]),
        )
        .await;
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    let old = fixture.login(MEMBER, PASSWORD, None).await;
    assert_eq!(location(&old), "/ui/login?error=credentials");
}

#[tokio::test]
async fn a_new_address_is_confirmed_without_a_mailer() {
    let fixture = Fixture::start().await;
    let token = placeholder(&fixture, MEMBER).await;

    let response = fixture
        .post(
            "/ui/confirm-email",
            Some(&token),
            &form(&[("email", "fresh@example.org")]),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let page = body_text(response).await;
    assert!(page.contains("Your address is confirmed."), "{page}");
    let user = db::user_by_email(&fixture.state.pool, "fresh@example.org")
        .await
        .unwrap()
        .expect("the address moved");
    assert!(user.is_verified);
}

#[tokio::test]
async fn with_a_mailer_the_address_is_asked_to_verify() {
    let fixture = Fixture::start_with_mail().await;
    let token = placeholder(&fixture, MEMBER).await;

    let response = fixture
        .post(
            "/ui/confirm-email",
            Some(&token),
            &form(&[("email", "fresh@example.org")]),
        )
        .await;

    let page = body_text(response).await;
    assert!(
        page.contains("Check your inbox to verify fresh@example.org."),
        "{page}"
    );
    let mail = fixture.wait_for_mail("fresh@example.org").await;
    assert!(mail.contains("/ui/verify#"), "{mail}");
    let user = db::user_by_email(&fixture.state.pool, "fresh@example.org")
        .await
        .unwrap()
        .expect("the address moved");
    assert!(!user.is_verified);
}

#[tokio::test]
async fn an_address_owned_by_somebody_else_is_reported() {
    let fixture = Fixture::start().await;
    fixture.verified("taken@example.org").await;
    let token = placeholder(&fixture, MEMBER).await;

    let response = fixture
        .post(
            "/ui/confirm-email",
            Some(&token),
            &form(&[("email", "taken@example.org")]),
        )
        .await;

    let page = body_text(response).await;
    assert!(
        page.contains("That address belongs to an existing account"),
        "{page}"
    );
}

#[tokio::test]
async fn an_empty_address_is_refused() {
    let fixture = Fixture::start().await;
    let token = placeholder(&fixture, MEMBER).await;

    let response = fixture
        .post("/ui/confirm-email", Some(&token), &form(&[("email", " ")]))
        .await;

    let page = body_text(response).await;
    assert!(page.contains("Enter your email address"), "{page}");
    assert!(
        db::user_by_email(&fixture.state.pool, PLACEHOLDER)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_placeholder_address_is_refused() {
    let fixture = Fixture::start().await;
    let token = placeholder(&fixture, MEMBER).await;

    let response = fixture
        .post(
            "/ui/confirm-email",
            Some(&token),
            &form(&[("email", PLACEHOLDER)]),
        )
        .await;

    let page = body_text(response).await;
    assert!(page.contains("Enter your email address"), "{page}");
}

#[tokio::test]
async fn a_malformed_address_is_refused() {
    let fixture = Fixture::start().await;
    let token = placeholder(&fixture, MEMBER).await;

    let response = fixture
        .post(
            "/ui/confirm-email",
            Some(&token),
            &form(&[("email", "not an address")]),
        )
        .await;

    let page = body_text(response).await;
    assert!(page.contains("That is not an email address"), "{page}");
}

#[tokio::test]
async fn a_proven_address_is_sent_to_the_account_page() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;

    let response = fixture.get("/ui/confirm-email", Some(&token)).await;

    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(location(&response), "/ui/account");
}

#[tokio::test]
async fn confirm_email_requires_login() {
    let fixture = Fixture::start().await;

    let response = fixture.get("/ui/confirm-email", None).await;

    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(location(&response), "/ui/login?next=%2Fui%2Fconfirm-email");
}

#[tokio::test]
async fn a_lost_session_is_reported_to_the_login_page() {
    let fixture = Fixture::start().await;
    let token = placeholder(&fixture, MEMBER).await;
    fixture.deactivate(PLACEHOLDER).await;

    let response = fixture
        .post(
            "/ui/confirm-email",
            Some(&token),
            &form(&[("email", "fresh@example.org")]),
        )
        .await;

    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert!(location(&response).starts_with("/ui/login"));
    assert!(
        db::user_by_email(&fixture.state.pool, "fresh@example.org")
            .await
            .unwrap()
            .is_none()
    );
}
