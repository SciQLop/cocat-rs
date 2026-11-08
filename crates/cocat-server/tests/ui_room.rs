mod ui_support;

use std::time::Duration;

use axum::http::StatusCode;
use axum::response::Response;
use cocat_core::role::Role;
use cocat_server::room::{CloseReason, outbox};
use cocat_server::{db, groups, invitations, lifecycle};
use ui_support::{Fixture, PUBLIC_URL, body_text, flash_cookie, form, location, share_link_cookie};
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

/// What the visitor sees after the POST: the old page sends them into the app, whose head
/// (or, without the app installed, its stand-in page) carries the notice the POST left.
async fn page_after(fixture: &Fixture, response: &Response, token: &str) -> String {
    assert!(
        flash_cookie(response).is_some(),
        "the action flashes a message"
    );
    fixture.follow(response, token).await.1
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
    let token = fixture.token(user).await;
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
    assert!(foreign_body.contains("Project not found"), "{foreign_body}");
    assert!(
        foreign_body.contains(r#"href="/ui/rooms""#),
        "{foreign_body}"
    );
    assert!(
        foreign_body.contains("Back to my projects"),
        "{foreign_body}"
    );
}

#[tokio::test]
async fn every_member_is_sent_to_the_share_dialog() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let id = room_owned_by(&fixture, user, "lab").await;
    for (email, role) in [(OTHER, Role::Editor), (GUEST, Role::Viewer)] {
        member_of(&fixture, &id, email, role).await;
    }

    for email in [MEMBER, OTHER, GUEST] {
        let member = db::user_by_email(&fixture.state.pool, email)
            .await
            .unwrap()
            .unwrap();
        let token = fixture.token(member.id).await;
        let response = fixture.get(&format!("/ui/rooms/{id}"), Some(&token)).await;

        assert_eq!(response.status(), StatusCode::SEE_OTHER, "{email}");
        assert_eq!(location(&response), format!("/ui/rooms/{id}/browse#share"));
    }
}

#[tokio::test]
async fn change_role_closes_their_socket() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;
    let id = room_owned_by(&fixture, user, "lab").await;
    let member = member_of(&fixture, &id, GUEST, Role::Editor).await;
    let room = fixture.state.rooms.get_or_open(&id).unwrap();
    let (out, mut inbox) = outbox();
    room.attach(1, member, "test@example.org".to_owned(), Role::Editor, out)
        .unwrap();

    let response = fixture
        .post(
            &format!("/ui/rooms/{id}/members/{member}/role"),
            Some(&token),
            &form(&[("role", "viewer")]),
        )
        .await;

    assert_eq!(location(&response), format!("/ui/rooms/{id}"));
    let page = page_after(&fixture, &response, &token).await;
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
    let token = fixture.token(user).await;
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
    let page = page_after(&fixture, &response, &token).await;
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
    let token = fixture.token(user).await;
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
    let page = page_after(&fixture, &response, &token).await;
    assert!(
        page.contains(&format!("{OTHER} now owns the project")),
        "{page}"
    );
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
    let token = fixture.token(user).await;
    let id = room_owned_by(&fixture, other, "theirs").await;
    member_of(&fixture, &id, MEMBER, Role::Editor).await;

    let response = fixture
        .post(&format!("/ui/rooms/{id}/leave"), Some(&token), "")
        .await;

    assert_eq!(location(&response), "/ui/rooms");
    let page = page_after(&fixture, &response, &token).await;
    assert!(page.contains("You left the project"), "{page}");
    assert_eq!(
        db::role_in(&fixture.state.pool, &id, user).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn share_link_uses_public_url() {
    let fixture = Fixture::start_with_mail().await.with_app();

    let page = share_link_page(&fixture, Role::Editor).await;

    assert!(
        page.contains(r#"<meta name="cocat-flash" content="Share link created">"#),
        "{page}"
    );
    let link = format!(r#"<meta name="cocat-share-link" content="{PUBLIC_URL}/ui/invitation#"#);
    assert!(page.contains(&link), "{page}");
}

/// A share link rides in a cookie of its own: whatever a notice says, the field it fills
/// holds only the link the create action handed back.
#[tokio::test]
async fn share_link_survives_a_newline_free_flash() {
    let fixture = Fixture::start_with_mail().await.with_app();
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;
    let id = room_owned_by(&fixture, user, "lab").await;
    let path = format!("/ui/rooms/{id}");

    let response = fixture
        .post(
            &format!("{path}/invitations"),
            Some(&token),
            &form(&[("role", "viewer"), ("kind", "link")]),
        )
        .await;

    let flash = flash_cookie(&response).expect("the action flashes a notice");
    assert!(flash.contains("Share%20link%20created"), "{flash}");
    assert!(
        !flash.contains("%0A"),
        "the notice carries no second line: {flash}"
    );
    assert!(
        !flash.contains("/invitation#"),
        "the link is not in the notice: {flash}"
    );
    let link = share_link_cookie(&response).expect("the link has its own cookie");
    let decoded = cookie::Cookie::parse_encoded(&link)
        .unwrap()
        .value()
        .to_owned();
    assert!(
        decoded.starts_with(&format!("{PUBLIC_URL}/ui/invitation#")),
        "{decoded}"
    );

    let forged = "cocat_flash=Shared%20with%20a%0Ab";
    let app = format!("{path}/browse");
    let page = body_text(
        fixture
            .get_with_cookies(&app, &token, &[forged, &link])
            .await,
    )
    .await;

    assert!(page.contains("Shared with a"), "{page}");
    assert!(
        page.contains(&format!(
            r#"<meta name="cocat-share-link" content="{decoded}">"#
        )),
        "{page}"
    );
}

/// Create a share link as the owner and read the page the flash redirects to.
async fn share_link_page(fixture: &Fixture, role: Role) -> String {
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;
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
    page_after(fixture, &response, &token).await
}

#[tokio::test]
async fn revoke() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;
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
    let page = page_after(&fixture, &response, &token).await;
    assert!(page.contains("Invitation revoked"), "{page}");
    assert!(
        invitations::list(&fixture.state, &id, user)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn invite_by_email_with_mailer() {
    let fixture = Fixture::start_with_mail().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;
    let id = room_owned_by(&fixture, user, "lab").await;

    let response = fixture
        .post(
            &format!("/ui/rooms/{id}/invitations"),
            Some(&token),
            &form(&[("role", "editor"), ("kind", "email"), ("email", GUEST)]),
        )
        .await;

    let page = page_after(&fixture, &response, &token).await;
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
    let fixture = Fixture::start().await.with_app();
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;
    let id = room_owned_by(&fixture, user, "lab").await;

    let response = fixture
        .post(
            &format!("/ui/rooms/{id}/invitations"),
            Some(&token),
            &form(&[("role", "viewer"), ("kind", "email"), ("email", GUEST)]),
        )
        .await;

    let page = page_after(&fixture, &response, &token).await;
    assert!(page.contains("Invitation created"), "{page}");
    let base = fixture.state.links.ui("");
    assert!(
        page.contains(&format!(
            r#"<meta name="cocat-share-link" content="{base}/invitation#"#
        )),
        "{page}"
    );
    let pending = invitations::list(&fixture.state, &id, user).await.unwrap();
    assert_eq!(pending[0].email.as_deref(), Some(GUEST));
}

#[tokio::test]
async fn refused_after_losing_access() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let other = fixture.verified(OTHER).await;
    let token = fixture.token(user).await;
    let id = room_owned_by(&fixture, other, "theirs").await;
    member_of(&fixture, &id, MEMBER, Role::Editor).await;
    db::remove_member(&fixture.state.pool, &id, user)
        .await
        .unwrap();

    let response = fixture
        .post(&format!("/ui/rooms/{id}/leave"), Some(&token), "")
        .await;

    assert_eq!(location(&response), "/ui/rooms");
    let page = page_after(&fixture, &response, &token).await;
    assert!(page.contains(&format!("Project {id} not found")), "{page}");
}

#[tokio::test]
async fn deactivated_session_is_refused() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user).await;
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
    let token = fixture.token(user).await;
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
    let page = page_after(&fixture, &response, &token).await;
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

async fn group_owned_by(fixture: &Fixture, owner: Uuid, name: &str) -> String {
    groups::create(&fixture.state, owner, name)
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn share_with_group() {
    let fixture = Fixture::start().await;
    let owner = fixture.verified(MEMBER).await;
    let room = room_owned_by(&fixture, owner, "lab").await;
    let group = group_owned_by(&fixture, owner, "friends").await;
    let token = fixture.token(owner).await;

    let response = fixture
        .post(
            &format!("/ui/rooms/{room}/groups"),
            Some(&token),
            &form(&[("group", &group), ("role", "viewer")]),
        )
        .await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), format!("/ui/rooms/{room}"));
    let flash = flash_cookie(&response).expect("sharing flashes a message");
    assert!(flash.contains("Shared%20with%20friends"), "{flash}");
    let grants = groups::room_groups(&fixture.state, &room, owner)
        .await
        .unwrap();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].0.name, "friends");
    assert_eq!(grants[0].1, Role::Viewer);
}

#[tokio::test]
async fn change_the_role_of_a_granted_group() {
    let fixture = Fixture::start().await;
    let owner = fixture.verified(MEMBER).await;
    let room = room_owned_by(&fixture, owner, "lab").await;
    let group = group_owned_by(&fixture, owner, "friends").await;
    groups::grant(&fixture.state, &room, &group, owner, Role::Editor)
        .await
        .unwrap();
    let token = fixture.token(owner).await;

    let response = fixture
        .post(
            &format!("/ui/rooms/{room}/groups"),
            Some(&token),
            &form(&[("group", &group), ("role", "viewer")]),
        )
        .await;

    let flash = flash_cookie(&response).expect("the change flashes a message");
    assert!(flash.contains("friends%20is%20now%20viewer"), "{flash}");
    let grants = groups::room_groups(&fixture.state, &room, owner)
        .await
        .unwrap();
    assert_eq!(grants[0].1, Role::Viewer);
}

#[tokio::test]
async fn stop_sharing_with_a_group() {
    let fixture = Fixture::start().await;
    let owner = fixture.verified(MEMBER).await;
    let room = room_owned_by(&fixture, owner, "lab").await;
    let group = group_owned_by(&fixture, owner, "friends").await;
    groups::grant(&fixture.state, &room, &group, owner, Role::Editor)
        .await
        .unwrap();
    let token = fixture.token(owner).await;

    let response = fixture
        .post(
            &format!("/ui/rooms/{room}/groups/{group}/remove"),
            Some(&token),
            "",
        )
        .await;

    let flash = flash_cookie(&response).expect("the removal flashes a message");
    assert!(
        flash.contains("Stopped%20sharing%20with%20friends"),
        "{flash}"
    );
    assert!(
        groups::room_groups(&fixture.state, &room, owner)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_foreign_group_is_refused() {
    let fixture = Fixture::start().await;
    let owner = fixture.verified(MEMBER).await;
    let other = fixture.verified(OTHER).await;
    let room = room_owned_by(&fixture, owner, "lab").await;
    let foreign = group_owned_by(&fixture, other, "theirs").await;
    let token = fixture.token(owner).await;

    let response = fixture
        .post(
            &format!("/ui/rooms/{room}/groups"),
            Some(&token),
            &form(&[("group", &foreign), ("role", "viewer")]),
        )
        .await;

    let flash = flash_cookie(&response).expect("the refusal flashes a message");
    assert!(
        flash.contains(&format!("Group%20{foreign}%20not%20found")),
        "{flash}"
    );
    assert!(
        groups::room_groups(&fixture.state, &room, owner)
            .await
            .unwrap()
            .is_empty()
    );
}
