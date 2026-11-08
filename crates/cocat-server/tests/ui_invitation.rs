mod ui_support;

use axum::http::StatusCode;
use cocat_core::role::Role;
use cocat_server::{db, invitations, lifecycle};
use ui_support::{Fixture, body_text, flash_cookie, form, location};

const MEMBER: &str = "member@example.org";
const OWNER: &str = "owner@example.org";
const OTHER: &str = "other@example.org";

struct Invited {
    room_id: String,
    room_name: String,
    token: String,
    expires: String,
}

/// A room owned by someone else, with an invitation for the fixture's guest.
async fn invitation(fixture: &Fixture, email: Option<&str>, max_uses: Option<u32>) -> Invited {
    let owner = fixture.verified(OWNER).await;
    let room = lifecycle::create_room(&fixture.state, owner, "lab")
        .await
        .unwrap();
    let (_, token) = invitations::create(
        &fixture.state,
        &room.id,
        owner,
        Role::Editor,
        email,
        14,
        max_uses,
    )
    .await
    .unwrap();
    let listed = invitations::list(&fixture.state, &room.id, owner)
        .await
        .unwrap();
    let expires = listed
        .first()
        .expect("the invitation is usable")
        .expires_at
        .chars()
        .take(10)
        .collect();
    Invited {
        room_id: room.id,
        room_name: room.name,
        token,
        expires,
    }
}

/// The message a redirect left in the flash cookie, decoded.
fn flashed(response: &axum::response::Response) -> String {
    let raw = flash_cookie(response).expect("a message");
    cookie::Cookie::parse_encoded(&raw)
        .expect("a cookie")
        .value()
        .to_owned()
}

#[tokio::test]
async fn a_link_without_a_token_offers_the_paste_box() {
    let fixture = Fixture::start().await;

    let page = body_text(fixture.get("/ui/invitation", None).await).await;

    assert!(page.contains("Join a project"), "{page}");
    assert!(page.contains(r#"data-submit-fragment"#), "{page}");
    assert!(
        page.contains(r#"<label for="link">Invitation link</label>"#),
        "{page}"
    );
    assert!(page.contains(r#"name="link""#), "{page}");
    assert!(page.contains(">Continue<"), "{page}");
    assert!(!page.contains(">Accept<"), "{page}");
}

#[tokio::test]
async fn preview_shows_what_you_are_joining() {
    let fixture = Fixture::start().await;
    let invited = invitation(&fixture, None, None).await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;

    let response = fixture
        .post(
            "/ui/invitation",
            Some(&token),
            &form(&[("token", &invited.token)]),
        )
        .await;

    let page = body_text(response).await;
    assert!(
        page.contains(&format!("{OWNER} invites you to {}", invited.room_name)),
        "{page}"
    );
    assert!(page.contains("You join as editor"), "{page}");
    assert!(
        page.contains(&format!("The invitation lasts until {}", invited.expires)),
        "{page}"
    );
    assert!(page.contains(">Accept<"), "{page}");
}

#[tokio::test]
async fn accept_joins_the_room() {
    let fixture = Fixture::start().await;
    let invited = invitation(&fixture, None, None).await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;

    let response = fixture
        .post(
            "/ui/invitation/accept",
            Some(&token),
            &form(&[("token", &invited.token)]),
        )
        .await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        location(&response),
        format!("/ui/rooms/{}/browse", invited.room_id),
        "the app opens on the project joined"
    );
    assert_eq!(
        db::role_in(&fixture.state.pool, &invited.room_id, user)
            .await
            .unwrap(),
        Some(Role::Editor)
    );
    let flash = flashed(&response);
    assert!(
        flash.contains(&format!("You joined {}", invited.room_name)),
        "{flash}"
    );
}

#[tokio::test]
async fn a_second_accept_when_exhausted_is_refused() {
    let fixture = Fixture::start().await;
    let invited = invitation(&fixture, None, Some(1)).await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;

    fixture
        .post(
            "/ui/invitation/accept",
            Some(&token),
            &form(&[("token", &invited.token)]),
        )
        .await;

    let response = fixture
        .post(
            "/ui/invitation",
            Some(&token),
            &form(&[("token", &invited.token)]),
        )
        .await;

    let page = body_text(response).await;
    assert!(
        page.contains("This invitation is not valid anymore"),
        "{page}"
    );
    assert!(!page.contains(">Accept<"), "{page}");
}

#[tokio::test]
async fn an_addressed_invitation_refuses_another_address() {
    let fixture = Fixture::start().await;
    let invited = invitation(&fixture, Some(OTHER), None).await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;

    let response = fixture
        .post(
            "/ui/invitation/accept",
            Some(&token),
            &form(&[("token", &invited.token)]),
        )
        .await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/ui/invitation");
    let flash = flashed(&response);
    assert!(
        flash.contains("This invitation is for another email address"),
        "{flash}"
    );
    assert_eq!(
        db::role_in(&fixture.state.pool, &invited.room_id, user)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn an_invalid_token_is_reported() {
    let fixture = Fixture::start().await;

    let response = fixture
        .post("/ui/invitation", None, &form(&[("token", "nope")]))
        .await;

    let page = body_text(response).await;
    assert!(
        page.contains("This invitation is not valid anymore"),
        "{page}"
    );
    assert!(!page.contains(">Accept<"), "{page}");
}

#[tokio::test]
async fn a_pasted_link_is_read() {
    let fixture = Fixture::start().await;
    let invited = invitation(&fixture, None, None).await;

    let link = format!("https://cocat.example.org/ui/invitation#{}", invited.token);
    let response = fixture
        .post("/ui/invitation", None, &form(&[("link", &link)]))
        .await;

    let page = body_text(response).await;
    assert!(page.contains(&invited.room_name), "{page}");
    assert!(page.contains("You join as editor"), "{page}");
}

#[tokio::test]
async fn a_pasted_bare_token_is_read() {
    let fixture = Fixture::start().await;
    let invited = invitation(&fixture, None, None).await;

    let response = fixture
        .post("/ui/invitation", None, &form(&[("link", &invited.token)]))
        .await;

    let page = body_text(response).await;
    assert!(page.contains(&invited.room_name), "{page}");
}

#[tokio::test]
async fn a_logged_out_visitor_is_sent_to_the_login_page() {
    let fixture = Fixture::start().await;
    let invited = invitation(&fixture, None, None).await;

    let response = fixture
        .post("/ui/invitation", None, &form(&[("token", &invited.token)]))
        .await;

    let page = body_text(response).await;
    assert!(page.contains(&invited.room_name), "{page}");
    assert!(!page.contains(">Accept<"), "{page}");
    assert!(
        page.contains(r#"href="/ui/login?next=%2Fui%2Finvitation""#),
        "{page}"
    );
    assert!(
        page.contains("Signing in leaves this page behind: paste the link again."),
        "{page}"
    );
    assert!(!page.contains(&invited.token), "{page}");
}

#[tokio::test]
async fn the_token_travels_only_in_post_bodies() {
    let fixture = Fixture::start().await;
    let invited = invitation(&fixture, None, None).await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;

    let page = body_text(
        fixture
            .post(
                "/ui/invitation",
                Some(&token),
                &form(&[("token", &invited.token)]),
            )
            .await,
    )
    .await;

    assert!(
        page.contains(r#"<form method="post" action="/ui/invitation/accept">"#),
        "{page}"
    );
    assert!(
        page.contains(&format!(
            r#"<input type="hidden" name="token" value="{}">"#,
            invited.token
        )),
        "{page}"
    );
    assert!(
        !page.contains(&format!(r#"action="/ui/invitation/{}"#, invited.token)),
        "{page}"
    );
    assert!(
        !page.contains(&format!(r#"?token={}"#, invited.token)),
        "{page}"
    );
    assert!(!page.contains(&format!(r#"#{}"#, invited.token)), "{page}");
}
