mod ui_support;

use std::time::Duration;

use axum::http::StatusCode;
use axum::response::Response;
use cocat_core::role::Role;
use cocat_server::room::{CloseReason, outbox};
use cocat_server::{db, invitations, lifecycle};
use ui_support::{Fixture, PUBLIC_URL, body_text, flash_cookie, form, location};
use uuid::Uuid;

const MEMBER: &str = "member@example.org";
const OTHER: &str = "other@example.org";
const GUEST: &str = "guest@example.org";

async fn room_owned_by(fixture: &Fixture, owner: Uuid, name: &str) -> String {
    lifecycle::create_room(&fixture.state, owner, name)
        .await
        .unwrap()
        .id
}

/// A member of the room, created if the fixture has not seen the address yet.
async fn member_of(fixture: &Fixture, room_id: &str, email: &str, role: Role) -> Uuid {
    let id = match db::user_by_email(&fixture.state.pool, email).await.unwrap() {
        Some(user) => user.id,
        None => fixture.verified(email).await,
    };
    db::add_member(&fixture.state.pool, room_id, id, role)
        .await
        .unwrap();
    id
}

/// The page the POST redirects to, carrying the flash the POST left.
async fn page_after(fixture: &Fixture, response: &Response, token: &str, path: &str) -> String {
    let cookie = flash_cookie(response).expect("the action flashes a message");
    body_text(fixture.get_with(path, token, &cookie).await).await
}

#[tokio::test]
async fn login_required() {
    let fixture = Fixture::start().await;

    let response = fixture.get("/ui/rooms/lab", None).await;

    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(location(&response), "/ui/login?next=%2Fui%2Frooms%2Flab");
}

#[tokio::test]
async fn not_found_for_strangers() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let other = fixture.verified(OTHER).await;
    let token = fixture.token(user);
    let foreign = room_owned_by(&fixture, other, "theirs").await;

    let foreign_page = fixture
        .get(&format!("/ui/rooms/{foreign}"), Some(&token))
        .await;
    let unknown_page = fixture.get("/ui/rooms/nope", Some(&token)).await;

    assert_eq!(foreign_page.status(), StatusCode::NOT_FOUND);
    assert_eq!(unknown_page.status(), StatusCode::NOT_FOUND);
    let foreign_body = body_text(foreign_page).await;
    let unknown_body = body_text(unknown_page).await;
    assert_eq!(foreign_body, unknown_body, "one page for both");
    assert!(foreign_body.contains("Room not found"), "{foreign_body}");
    assert!(
        foreign_body.contains(r#"href="/ui/rooms""#),
        "{foreign_body}"
    );
    assert!(foreign_body.contains("Back to my rooms"), "{foreign_body}");
}

#[tokio::test]
async fn owner_view() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user);
    let id = room_owned_by(&fixture, user, "lab").await;
    let member = member_of(&fixture, &id, OTHER, Role::Editor).await;

    let page = body_text(fixture.get(&format!("/ui/rooms/{id}"), Some(&token)).await).await;

    assert!(page.contains("lab"), "{page}");
    assert!(page.contains("Your role: owner"), "{page}");
    assert!(page.contains("Members"), "{page}");
    assert!(page.contains(MEMBER), "{page}");
    assert!(page.contains(OTHER), "{page}");
    assert!(!page.contains(&format!("/ui/rooms/{id}/leave")), "{page}");
    assert!(page.contains("Create share link"), "{page}");
    assert!(page.contains(r#"name="email""#), "{page}");
    assert!(page.contains(&format!(r#"data-role="{member}""#)), "{page}");
    assert!(
        page.contains(&format!(r#"data-confirm="Remove {OTHER}?""#)),
        "{page}"
    );
    assert!(
        page.contains(&format!(r#"data-confirm="Make {OTHER} the owner?""#)),
        "{page}"
    );
    assert!(
        !page.contains(&format!(r#"data-role="{user}""#)),
        "the owner has no role picker: {page}"
    );
}

#[tokio::test]
async fn editor_view_has_no_role_picker() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let other = fixture.verified(OTHER).await;
    let token = fixture.token(user);
    let id = room_owned_by(&fixture, other, "theirs").await;
    member_of(&fixture, &id, MEMBER, Role::Editor).await;

    let page = body_text(fixture.get(&format!("/ui/rooms/{id}"), Some(&token)).await).await;

    assert!(page.contains("Your role: editor"), "{page}");
    assert!(page.contains(&format!("/ui/rooms/{id}/leave")), "{page}");
    assert!(
        page.contains("Only the owner can manage invitations."),
        "{page}"
    );
    assert!(!page.contains("Create share link"), "{page}");
    assert!(!page.contains(r#"name="role""#), "{page}");
    assert!(
        !page.contains(&format!("/ui/rooms/{id}/members/{other}/remove")),
        "{page}"
    );
}

#[tokio::test]
async fn viewer_view() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let other = fixture.verified(OTHER).await;
    let token = fixture.token(user);
    let id = room_owned_by(&fixture, other, "theirs").await;
    member_of(&fixture, &id, MEMBER, Role::Viewer).await;

    let page = body_text(fixture.get(&format!("/ui/rooms/{id}"), Some(&token)).await).await;

    assert!(page.contains("Your role: viewer"), "{page}");
    assert!(!page.contains(&format!(r#"data-role="{other}""#)), "{page}");
    assert!(page.contains(OTHER), "{page}");
}

#[tokio::test]
async fn change_role_closes_their_socket() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user);
    let id = room_owned_by(&fixture, user, "lab").await;
    let member = member_of(&fixture, &id, GUEST, Role::Editor).await;
    let room = fixture.state.rooms.get_or_open(&id).unwrap();
    let (out, mut inbox) = outbox();
    room.attach(1, member, Role::Editor, out).unwrap();

    let response = fixture
        .post(
            &format!("/ui/rooms/{id}/members/{member}/role"),
            Some(&token),
            &form(&[("role", "viewer")]),
        )
        .await;

    assert_eq!(location(&response), format!("/ui/rooms/{id}"));
    let page = page_after(&fixture, &response, &token, &format!("/ui/rooms/{id}")).await;
    assert!(page.contains(&format!("{GUEST} is now a viewer")), "{page}");
    let closed = tokio::time::timeout(Duration::from_secs(5), &mut inbox.close)
        .await
        .expect("the room closed the socket of the member whose role changed");
    assert_eq!(closed.ok(), Some(CloseReason::Revoked));
    assert_eq!(
        db::role_in(&fixture.state.pool, &id, member).await.unwrap(),
        Some(Role::Viewer)
    );
}

#[tokio::test]
async fn remove() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user);
    let id = room_owned_by(&fixture, user, "lab").await;
    let member = member_of(&fixture, &id, GUEST, Role::Editor).await;

    let response = fixture
        .post(
            &format!("/ui/rooms/{id}/members/{member}/remove"),
            Some(&token),
            "",
        )
        .await;

    assert_eq!(location(&response), format!("/ui/rooms/{id}"));
    let page = page_after(&fixture, &response, &token, &format!("/ui/rooms/{id}")).await;
    assert!(page.contains(&format!("Removed {GUEST}")), "{page}");
    assert_eq!(
        db::role_in(&fixture.state.pool, &id, member).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn transfer() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user);
    let id = room_owned_by(&fixture, user, "lab").await;
    let member = member_of(&fixture, &id, OTHER, Role::Editor).await;

    let response = fixture
        .post(
            &format!("/ui/rooms/{id}/members/{member}/owner"),
            Some(&token),
            "",
        )
        .await;

    assert_eq!(location(&response), format!("/ui/rooms/{id}"));
    let page = page_after(&fixture, &response, &token, &format!("/ui/rooms/{id}")).await;
    assert!(
        page.contains(&format!("{OTHER} now owns the room")),
        "{page}"
    );
    assert!(page.contains("Your role: editor"), "{page}");
    assert!(!page.contains("Create share link"), "{page}");
    assert_eq!(
        db::role_in(&fixture.state.pool, &id, member).await.unwrap(),
        Some(Role::Owner)
    );
    assert_eq!(
        db::role_in(&fixture.state.pool, &id, user).await.unwrap(),
        Some(Role::Editor)
    );
}

#[tokio::test]
async fn leave() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let other = fixture.verified(OTHER).await;
    let token = fixture.token(user);
    let id = room_owned_by(&fixture, other, "theirs").await;
    member_of(&fixture, &id, MEMBER, Role::Editor).await;

    let response = fixture
        .post(&format!("/ui/rooms/{id}/leave"), Some(&token), "")
        .await;

    assert_eq!(location(&response), "/ui/rooms");
    let page = body_text(
        fixture
            .get_with("/ui/rooms", &token, &flash_cookie(&response).unwrap())
            .await,
    )
    .await;
    assert!(page.contains("You left the room"), "{page}");
    assert_eq!(
        db::role_in(&fixture.state.pool, &id, user).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn share_link_uses_public_url() {
    let fixture = Fixture::start_with_mail().await;

    let page = share_link_page(&fixture, Role::Editor).await;

    assert!(page.contains("Share link created"), "{page}");
    let link = format!(r#"value="{PUBLIC_URL}/ui/invitation#"#);
    assert!(page.contains(&link), "{page}");
    assert!(page.contains("readonly"), "{page}");
    assert!(page.contains(">Copy<"), "{page}");
    assert!(page.contains("Share link"), "{page}");
}

/// Create a share link as the owner and read the page the flash redirects to.
async fn share_link_page(fixture: &Fixture, role: Role) -> String {
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user);
    let id = room_owned_by(fixture, user, "lab").await;
    let path = format!("/ui/rooms/{id}");
    let response = fixture
        .post(
            &format!("{path}/invitations"),
            Some(&token),
            &form(&[("role", role.as_str()), ("kind", "link")]),
        )
        .await;
    assert_eq!(location(&response), path, "{:?}", response.status());
    page_after(fixture, &response, &token, &path).await
}

#[tokio::test]
async fn revoke() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user);
    let id = room_owned_by(&fixture, user, "lab").await;
    let (invitation, _) =
        invitations::create(&fixture.state, &id, user, Role::Editor, None, 14, None)
            .await
            .unwrap();

    let response = fixture
        .post(
            &format!("/ui/rooms/{id}/invitations/{invitation}/revoke"),
            Some(&token),
            "",
        )
        .await;

    assert_eq!(location(&response), format!("/ui/rooms/{id}"));
    let page = page_after(&fixture, &response, &token, &format!("/ui/rooms/{id}")).await;
    assert!(page.contains("Invitation revoked"), "{page}");
    assert!(
        !page.contains(&format!("data-invitation=\"{invitation}\"")),
        "{page}"
    );
}

#[tokio::test]
async fn invite_by_email_with_mailer() {
    let fixture = Fixture::start_with_mail().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user);
    let id = room_owned_by(&fixture, user, "lab").await;

    let response = fixture
        .post(
            &format!("/ui/rooms/{id}/invitations"),
            Some(&token),
            &form(&[("role", "editor"), ("kind", "email"), ("email", GUEST)]),
        )
        .await;

    let page = page_after(&fixture, &response, &token, &format!("/ui/rooms/{id}")).await;
    assert!(
        page.contains(&format!("Invitation sent to {GUEST}")),
        "{page}"
    );
    assert!(!page.contains("/ui/invitation#"), "{page}");
    let mail = fixture.wait_for_mail(GUEST).await;
    assert!(mail.contains("/ui/invitation#"), "{mail}");
}

#[tokio::test]
async fn invite_by_email_without_mailer_shows_link() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user);
    let id = room_owned_by(&fixture, user, "lab").await;

    let response = fixture
        .post(
            &format!("/ui/rooms/{id}/invitations"),
            Some(&token),
            &form(&[("role", "viewer"), ("kind", "email"), ("email", GUEST)]),
        )
        .await;

    let page = page_after(&fixture, &response, &token, &format!("/ui/rooms/{id}")).await;
    assert!(page.contains("Invitation created"), "{page}");
    let base = fixture.state.links.ui("");
    assert!(
        page.contains(&format!(r#"value="{base}/invitation#"#)),
        "{page}"
    );
    assert!(page.contains(GUEST), "{page}");
}

#[tokio::test]
async fn refused_after_losing_access() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let other = fixture.verified(OTHER).await;
    let token = fixture.token(user);
    let id = room_owned_by(&fixture, other, "theirs").await;
    member_of(&fixture, &id, MEMBER, Role::Editor).await;
    db::remove_member(&fixture.state.pool, &id, user)
        .await
        .unwrap();

    let response = fixture
        .post(&format!("/ui/rooms/{id}/leave"), Some(&token), "")
        .await;

    assert_eq!(location(&response), "/ui/rooms");
    let page = body_text(
        fixture
            .get_with("/ui/rooms", &token, &flash_cookie(&response).unwrap())
            .await,
    )
    .await;
    assert!(page.contains(&format!("Room {id} not found")), "{page}");
}

#[tokio::test]
async fn deactivated_session_is_refused() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user);
    let id = room_owned_by(&fixture, user, "lab").await;
    fixture.deactivate(MEMBER).await;

    let response = fixture
        .post(
            &format!("/ui/rooms/{id}/invitations"),
            Some(&token),
            &form(&[("role", "editor"), ("kind", "link")]),
        )
        .await;

    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert!(location(&response).starts_with("/ui/login"), "{response:?}");
    assert!(
        invitations::list(&fixture.state, &id, user)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn revoke_foreign_invitation_is_refused() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let other = fixture.verified(OTHER).await;
    let token = fixture.token(user);
    let mine = room_owned_by(&fixture, user, "mine").await;
    let theirs = room_owned_by(&fixture, other, "theirs").await;
    let (invitation, _) =
        invitations::create(&fixture.state, &theirs, other, Role::Editor, None, 14, None)
            .await
            .unwrap();

    let response = fixture
        .post(
            &format!("/ui/rooms/{mine}/invitations/{invitation}/revoke"),
            Some(&token),
            "",
        )
        .await;

    assert_eq!(location(&response), format!("/ui/rooms/{mine}"));
    let page = page_after(&fixture, &response, &token, &format!("/ui/rooms/{mine}")).await;
    assert!(
        page.contains(&format!("Invitation {invitation} not found")),
        "{page}"
    );
    assert_eq!(
        invitations::list(&fixture.state, &theirs, other)
            .await
            .unwrap()
            .len(),
        1,
        "the foreign invitation is untouched"
    );
}

#[tokio::test]
async fn an_invitation_row_shows_usage() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user);
    let id = room_owned_by(&fixture, user, "lab").await;
    let guest = fixture.verified(GUEST).await;
    invitations::create(
        &fixture.state,
        &id,
        user,
        Role::Editor,
        Some(GUEST),
        14,
        None,
    )
    .await
    .unwrap();
    let (_, spent) =
        invitations::create(&fixture.state, &id, user, Role::Editor, None, 14, Some(2))
            .await
            .unwrap();
    let guest_user = db::user_by_id(&fixture.state.pool, guest)
        .await
        .unwrap()
        .unwrap();
    let (room, _) = invitations::accept(&fixture.state, &spent, &guest_user)
        .await
        .unwrap();
    assert_eq!(room.id, id);

    let page = body_text(fixture.get(&format!("/ui/rooms/{id}"), Some(&token)).await).await;

    assert!(page.contains(GUEST), "{page}");
    assert!(page.contains("Share link"), "{page}");
    assert!(page.contains("1/2"), "{page}");
    assert!(page.contains("0/1"), "{page}");
    let expires = invitations::list(&fixture.state, &id, user)
        .await
        .unwrap()
        .first()
        .map(|invitation| invitation.expires_at[..10].to_owned())
        .unwrap();
    assert!(page.contains(&expires), "{page}");
}
