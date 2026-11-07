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

/// The page the POST redirects to, carrying the notice (and share link) the POST left.
async fn page_after(fixture: &Fixture, response: &Response, token: &str, path: &str) -> String {
    let mut jar = vec![flash_cookie(response).expect("the action flashes a message")];
    jar.extend(share_link_cookie(response));
    let cookies: Vec<&str> = jar.iter().map(String::as_str).collect();
    body_text(fixture.get_with_cookies(path, token, &cookies).await).await
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
    let token = fixture.token(user).await;
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
    let token = fixture.token(user).await;
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
    let token = fixture.token(user).await;
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
    let token = fixture.token(user).await;
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
    let token = fixture.token(user).await;
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

/// A share link rides in a cookie of its own: whatever a notice says, the field it fills
/// holds only the link the create action handed back.
#[tokio::test]
async fn share_link_survives_a_newline_free_flash() {
    let fixture = Fixture::start_with_mail().await;
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
    let page = body_text(
        fixture
            .get_with_cookies(&path, &token, &[forged, &link])
            .await,
    )
    .await;

    assert!(page.contains("Shared with a"), "{page}");
    assert!(page.contains(&format!(r#"value="{decoded}""#)), "{page}");
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
    page_after(fixture, &response, &token, &path).await
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
    let token = fixture.token(user).await;
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
    let token = fixture.token(user).await;
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
    let token = fixture.token(user).await;
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

async fn group_owned_by(fixture: &Fixture, owner: Uuid, name: &str) -> String {
    groups::create(&fixture.state, owner, name)
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn grants_are_listed_for_viewers() {
    let fixture = Fixture::start().await;
    let owner = fixture.verified(MEMBER).await;
    let room = room_owned_by(&fixture, owner, "lab").await;
    let viewer = member_of(&fixture, &room, OTHER, Role::Viewer).await;
    let group = group_owned_by(&fixture, owner, "friends").await;
    groups::grant(&fixture.state, &room, &group, owner, Role::Editor)
        .await
        .unwrap();
    let token = fixture.token(viewer).await;

    let page = body_text(
        fixture
            .get(&format!("/ui/rooms/{room}"), Some(&token))
            .await,
    )
    .await;

    assert!(page.contains("friends"), "{page}");
    assert!(
        page.contains(r#"<span class="role">editor</span>"#),
        "{page}"
    );
    assert!(!page.contains(">Share with group<"), "{page}");
    assert!(
        !page.contains(&format!("data-group-role=\"{group}\"")),
        "{page}"
    );
}

#[tokio::test]
async fn the_owner_picker_lists_only_their_ungranted_groups() {
    let fixture = Fixture::start().await;
    let owner = fixture.verified(MEMBER).await;
    let other = fixture.verified(OTHER).await;
    let room = room_owned_by(&fixture, owner, "lab").await;
    let granted = group_owned_by(&fixture, owner, "granted").await;
    let spare = group_owned_by(&fixture, owner, "spare").await;
    let foreign = group_owned_by(&fixture, other, "foreign").await;
    groups::grant(&fixture.state, &room, &granted, owner, Role::Editor)
        .await
        .unwrap();
    let token = fixture.token(owner).await;

    let page = body_text(
        fixture
            .get(&format!("/ui/rooms/{room}"), Some(&token))
            .await,
    )
    .await;

    assert!(page.contains(">Share with group<"), "{page}");
    assert!(
        page.contains(&format!(r#"<option value="{spare}""#)),
        "{page}"
    );
    assert!(
        !page.contains(&format!(r#"<option value="{granted}""#)),
        "{page}"
    );
    assert!(
        !page.contains(&format!(r#"<option value="{foreign}""#)),
        "{page}"
    );
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

    let page = body_text(
        fixture
            .get(&format!("/ui/rooms/{room}"), Some(&token))
            .await,
    )
    .await;
    assert!(
        page.contains(r#"data-confirm="Stop sharing with friends?""#),
        "{page}"
    );

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

/// The email field suggests the people the owner already works with: members of their other
/// rooms and of their groups, never strangers, the owner, or members of this room.
#[tokio::test]
async fn the_email_field_suggests_people_the_owner_works_with() {
    let fixture = Fixture::start().await;
    let owner = fixture.verified(MEMBER).await;
    let room = room_owned_by(&fixture, owner, "lab").await;
    let other_room = room_owned_by(&fixture, owner, "elsewhere").await;
    member_of(&fixture, &room, OTHER, Role::Editor).await;
    member_of(&fixture, &other_room, OTHER, Role::Viewer).await;
    member_of(&fixture, &other_room, "colleague@example.org", Role::Viewer).await;
    let group = group_owned_by(&fixture, owner, "friends").await;
    let friend = fixture.verified("friend@example.org").await;
    sqlx::query("INSERT INTO group_member (group_id, user_id, role) VALUES (?, ?, 'member')")
        .bind(&group)
        .bind(friend.to_string())
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    fixture.verified("stranger@example.org").await;
    let token = fixture.token(owner).await;

    let page = body_text(
        fixture
            .get(&format!("/ui/rooms/{room}"), Some(&token))
            .await,
    )
    .await;

    assert!(page.contains(r#"list="known-people""#), "{page}");
    assert!(
        page.contains(r#"<option value="colleague@example.org">"#),
        "{page}"
    );
    assert!(
        page.contains(r#"<option value="friend@example.org">"#),
        "{page}"
    );
    assert!(
        !page.contains(r#"<option value="stranger@example.org">"#),
        "{page}"
    );
    assert!(
        !page.contains(&format!(r#"<option value="{OTHER}">"#)),
        "{page}"
    );
    assert!(
        !page.contains(&format!(r#"<option value="{MEMBER}">"#)),
        "{page}"
    );
}
