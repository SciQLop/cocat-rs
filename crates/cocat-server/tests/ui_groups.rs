mod ui_support;

use std::time::Duration;

use axum::http::StatusCode;
use axum::response::Response;
use cocat_core::role::Role;
use cocat_server::room::{CloseReason, outbox};
use cocat_server::{db, groups, lifecycle};
use ui_support::{Fixture, body_text, flash_cookie, form, location, share_link_cookie};
use uuid::Uuid;

const MEMBER: &str = "member@example.org";
const OWNER: &str = "owner@example.org";
const OTHER: &str = "other@example.org";
const GUEST: &str = "guest@example.org";

async fn group_owned_by(fixture: &Fixture, owner: Uuid, name: &str) -> String {
    groups::create(&fixture.state, owner, name)
        .await
        .unwrap()
        .id
}

/// A plain member of the group, created if the fixture has not seen the address yet.
async fn member_of(fixture: &Fixture, group_id: &str, email: &str) -> Uuid {
    let id = match db::user_by_email(&fixture.state.pool, email).await.unwrap() {
        Some(user) => user.id,
        None => fixture.verified(email).await,
    };
    sqlx::query("INSERT INTO group_member (group_id, user_id, role) VALUES (?, ?, 'member')")
        .bind(group_id)
        .bind(id.to_string())
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    id
}

fn flashed(response: &Response) -> String {
    let raw = flash_cookie(response).expect("a message");
    cookie::Cookie::parse_encoded(&raw)
        .expect("a cookie")
        .value()
        .to_owned()
}

/// The page the POST redirects to, carrying the notice the POST left.
async fn page_after(fixture: &Fixture, response: &Response, token: &str, path: &str) -> String {
    let cookies = [flash_cookie(response).expect("the action flashes a message")];
    let cookies: Vec<&str> = cookies.iter().map(String::as_str).collect();
    body_text(fixture.get_with_cookies(path, token, &cookies).await).await
}

#[tokio::test]
async fn login_required() {
    let fixture = Fixture::start().await;

    let list = fixture.get("/ui/groups", None).await;
    assert_eq!(list.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(location(&list), "/ui/login?next=%2Fui%2Fgroups");

    let page = fixture.get("/ui/groups/abc", None).await;
    assert_eq!(page.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(location(&page), "/ui/login?next=%2Fui%2Fgroups%2Fabc");
}

#[tokio::test]
async fn create_list_rename_delete() {
    let fixture = Fixture::start().await;
    let owner = fixture.verified(OWNER).await;
    let token = fixture.token(owner).await;

    let created = fixture
        .post("/ui/groups", Some(&token), &form(&[("name", "friends")]))
        .await;
    assert_eq!(location(&created), "/ui/groups");
    assert_eq!(flashed(&created), "Group created");

    let page = page_after(&fixture, &created, &token, "/ui/groups").await;
    assert!(page.contains("<h1>My groups</h1>"), "{page}");
    assert!(page.contains(r#"action="/ui/groups""#), "{page}");
    assert!(page.contains("New group name"), "{page}");
    assert!(page.contains(">Create<"), "{page}");
    assert!(page.contains("friends"), "{page}");
    assert!(
        page.contains(r#"<span class="role">owner</span>"#),
        "{page}"
    );
    assert!(page.contains(">Rename<"), "{page}");
    assert!(
        page.contains(
            "data-confirm=\"Delete friends? Its projects stop being shared with its members.\""
        ),
        "{page}"
    );
    let id = groups::list_for(&fixture.state, owner)
        .await
        .unwrap()
        .into_iter()
        .find(|(group, _)| group.name == "friends")
        .map(|(group, _)| group.id)
        .expect("the group was created");

    let renamed = fixture
        .post(
            &format!("/ui/groups/{id}/rename"),
            Some(&token),
            &form(&[("name", "colleagues")]),
        )
        .await;
    assert_eq!(flashed(&renamed), "Group renamed");
    let page = page_after(&fixture, &renamed, &token, "/ui/groups").await;
    assert!(page.contains("colleagues"), "{page}");
    assert!(!page.contains("friends"), "{page}");

    let deleted = fixture
        .post(&format!("/ui/groups/{id}/delete"), Some(&token), "")
        .await;
    assert_eq!(location(&deleted), "/ui/groups");
    assert_eq!(flashed(&deleted), "Group deleted");
    let page = page_after(&fixture, &deleted, &token, "/ui/groups").await;
    assert!(!page.contains(&format!("data-group=\"{id}\"")), "{page}");
}

#[tokio::test]
async fn leave() {
    let fixture = Fixture::start().await;
    let owner = fixture.verified(OWNER).await;
    let id = group_owned_by(&fixture, owner, "friends").await;
    let member = member_of(&fixture, &id, MEMBER).await;
    let token = fixture.token(member).await;

    let page = body_text(fixture.get(&format!("/ui/groups/{id}"), Some(&token)).await).await;
    assert!(page.contains("data-confirm=\"Leave friends?\""), "{page}");

    let response = fixture
        .post(&format!("/ui/groups/{id}/leave"), Some(&token), "")
        .await;
    assert_eq!(location(&response), "/ui/groups");
    assert_eq!(flashed(&response), "You left the group");

    let groups = groups::list_for(&fixture.state, member).await.unwrap();
    assert!(groups.is_empty(), "{groups:?}");
}

#[tokio::test]
async fn owner_cannot_leave() {
    let fixture = Fixture::start().await;
    let owner = fixture.verified(OWNER).await;
    let token = fixture.token(owner).await;
    let id = group_owned_by(&fixture, owner, "friends").await;

    let response = fixture
        .post(&format!("/ui/groups/{id}/leave"), Some(&token), "")
        .await;

    assert_eq!(location(&response), "/ui/groups");
    assert_eq!(flashed(&response), "The owner cannot leave the group");
    let groups = groups::list_for(&fixture.state, owner).await.unwrap();
    assert_eq!(groups.len(), 1);
}

#[tokio::test]
async fn members_and_remove() {
    let fixture = Fixture::start().await;
    let owner = fixture.verified(OWNER).await;
    let token = fixture.token(owner).await;
    let id = group_owned_by(&fixture, owner, "friends").await;
    let member = member_of(&fixture, &id, MEMBER).await;

    let page = body_text(fixture.get(&format!("/ui/groups/{id}"), Some(&token)).await).await;
    assert!(page.contains("Members"), "{page}");
    assert!(page.contains(OWNER), "{page}");
    assert!(page.contains(MEMBER), "{page}");
    assert!(page.contains("Your role: owner"), "{page}");
    assert!(
        page.contains(&format!("data-confirm=\"Remove {MEMBER}?\"")),
        "{page}"
    );

    let response = fixture
        .post(
            &format!("/ui/groups/{id}/members/{member}/remove"),
            Some(&token),
            "",
        )
        .await;
    assert_eq!(location(&response), format!("/ui/groups/{id}"));
    assert_eq!(flashed(&response), format!("Removed {MEMBER}"));
    let page = page_after(&fixture, &response, &token, &format!("/ui/groups/{id}")).await;
    assert!(
        !page.contains(&format!("data-member=\"{member}\"")),
        "{page}"
    );
}

#[tokio::test]
async fn remove_closes_sockets_in_shared_rooms() {
    let fixture = Fixture::start().await;
    let owner = fixture.verified(OWNER).await;
    let token = fixture.token(owner).await;
    let group = group_owned_by(&fixture, owner, "friends").await;
    let member = member_of(&fixture, &group, MEMBER).await;
    let room = lifecycle::create_room(&fixture.state, owner, "lab")
        .await
        .unwrap()
        .id;
    groups::grant(&fixture.state, &room, &group, owner, Role::Viewer)
        .await
        .unwrap();
    let open = fixture.state.rooms.get_or_open(&room).unwrap();
    let (out, mut inbox) = outbox();
    open.attach(1, member, "test@example.org".to_owned(), Role::Viewer, out)
        .unwrap();

    let response = fixture
        .post(
            &format!("/ui/groups/{group}/members/{member}/remove"),
            Some(&token),
            "",
        )
        .await;

    assert_eq!(flashed(&response), format!("Removed {MEMBER}"));
    let closed = tokio::time::timeout(Duration::from_secs(5), &mut inbox.close)
        .await
        .expect("the removal closed the socket the group carried");
    assert_eq!(closed.ok(), Some(CloseReason::Revoked));
    assert_eq!(
        db::role_in(&fixture.state.pool, &room, member)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn share_link_and_join() {
    let fixture = Fixture::start_with_mail().await;
    let owner = fixture.verified(OWNER).await;
    let owner_token = fixture.token(owner).await;
    let id = group_owned_by(&fixture, owner, "friends").await;
    let member = fixture.verified(MEMBER).await;
    let token = fixture.token(member).await;

    let created = fixture
        .post(
            &format!("/ui/groups/{id}/invitations"),
            Some(&owner_token),
            &form(&[("kind", "link")]),
        )
        .await;
    assert_eq!(location(&created), format!("/ui/groups/{id}"));
    assert_eq!(flashed(&created), "Share link created");
    let link = cookie::Cookie::parse_encoded(
        share_link_cookie(&created).expect("the share link is shown once"),
    )
    .unwrap()
    .value()
    .to_owned();
    assert!(link.contains("/ui/groups/join#"), "{link}");
    let invitation = link.split('#').nth(1).expect("a token").to_owned();

    let page = body_text(
        fixture
            .get_with("/ui/groups/join", &token, &flash_cookie(&created).unwrap())
            .await,
    )
    .await;
    assert!(page.contains("Join a group"), "{page}");
    assert!(page.contains("Invitation link"), "{page}");

    let preview = fixture
        .post(
            "/ui/groups/join",
            Some(&token),
            &form(&[("token", &invitation)]),
        )
        .await;
    assert_eq!(preview.status(), StatusCode::OK);
    let page = body_text(preview).await;
    assert!(
        page.contains(&format!("{OWNER} invites you to the group friends")),
        "{page}"
    );
    assert!(page.contains("The invitation lasts until"), "{page}");
    assert!(page.contains(">Accept<"), "{page}");

    let accepted = fixture
        .post(
            "/ui/groups/join/accept",
            Some(&token),
            &form(&[("token", &invitation)]),
        )
        .await;
    assert_eq!(accepted.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&accepted), format!("/ui/groups/{id}"));
    assert_eq!(flashed(&accepted), "You joined friends");
    let groups = groups::list_for(&fixture.state, member).await.unwrap();
    assert!(
        groups
            .iter()
            .any(|(group, role)| group.id == id && *role == groups::GroupRole::Member),
        "{groups:?}"
    );
}

#[tokio::test]
async fn invite_by_email() {
    let fixture = Fixture::start_with_mail().await;
    let owner = fixture.verified(OWNER).await;
    let token = fixture.token(owner).await;
    let id = group_owned_by(&fixture, owner, "friends").await;

    let response = fixture
        .post(
            &format!("/ui/groups/{id}/invitations"),
            Some(&token),
            &form(&[("kind", "email"), ("email", GUEST)]),
        )
        .await;

    assert_eq!(location(&response), format!("/ui/groups/{id}"));
    assert_eq!(flashed(&response), format!("Invitation sent to {GUEST}"));
    let mail = fixture.wait_for_mail(GUEST).await;
    let link = mail.replace("=\r\n", "").replace("=\n", "");
    assert!(link.contains("/ui/groups/join#"), "{mail}");
    assert!(!link.contains("/ui/groups/join/"), "{mail}");
    let page = page_after(&fixture, &response, &token, &format!("/ui/groups/{id}")).await;
    assert!(page.contains(GUEST), "{page}");
}

#[tokio::test]
async fn revoke() {
    let fixture = Fixture::start().await;
    let owner = fixture.verified(OWNER).await;
    let token = fixture.token(owner).await;
    let id = group_owned_by(&fixture, owner, "friends").await;
    let (invitation, _) =
        groups::create_invitation(&fixture.state, &id, owner, Some(GUEST), 14, None)
            .await
            .unwrap();

    let page = body_text(fixture.get(&format!("/ui/groups/{id}"), Some(&token)).await).await;
    assert!(
        page.contains("data-confirm=\"Revoke this invitation?\""),
        "{page}"
    );

    let response = fixture
        .post(
            &format!("/ui/groups/{id}/invitations/{invitation}/revoke"),
            Some(&token),
            "",
        )
        .await;
    assert_eq!(flashed(&response), "Invitation revoked");
    let page = page_after(&fixture, &response, &token, &format!("/ui/groups/{id}")).await;
    assert!(
        !page.contains(&format!("data-invitation=\"{invitation}\"")),
        "{page}"
    );
}

#[tokio::test]
async fn not_found_for_strangers() {
    let fixture = Fixture::start().await;
    let owner = fixture.verified(OWNER).await;
    let id = group_owned_by(&fixture, owner, "friends").await;
    let stranger = fixture.verified(MEMBER).await;
    let token = fixture.token(stranger).await;

    let page = fixture.get(&format!("/ui/groups/{id}"), Some(&token)).await;

    assert_eq!(page.status(), StatusCode::NOT_FOUND);
    let page = body_text(page).await;
    assert!(page.contains("Group not found"), "{page}");
    assert!(page.contains(r#"href="/ui/groups""#), "{page}");
    assert!(page.contains("Back to my groups"), "{page}");

    let missing = fixture.get("/ui/groups/nope", Some(&token)).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn remove_from_foreign_group_refused() {
    let fixture = Fixture::start().await;
    let owner = fixture.verified(OWNER).await;
    let token = fixture.token(owner).await;
    let theirs = group_owned_by(&fixture, fixture.verified(OTHER).await, "theirs").await;
    let member = member_of(&fixture, &theirs, GUEST).await;

    let response = fixture
        .post(
            &format!("/ui/groups/{theirs}/members/{member}/remove"),
            Some(&token),
            "",
        )
        .await;

    assert_eq!(location(&response), "/ui/groups");
    assert_eq!(flashed(&response), format!("Group {theirs} not found"));
    let members = groups::members(&fixture.state, &theirs, member)
        .await
        .unwrap();
    assert!(members.iter().any(|entry| entry.user_id == member));
}

#[tokio::test]
async fn rooms_shared_with_group_listed() {
    let fixture = Fixture::start().await;
    let owner = fixture.verified(OWNER).await;
    let token = fixture.token(owner).await;
    let id = group_owned_by(&fixture, owner, "friends").await;
    let room = lifecycle::create_room(&fixture.state, owner, "lab")
        .await
        .unwrap()
        .id;
    groups::grant(&fixture.state, &room, &id, owner, Role::Editor)
        .await
        .unwrap();

    let page = body_text(fixture.get(&format!("/ui/groups/{id}"), Some(&token)).await).await;

    assert!(page.contains("Projects shared with this group"), "{page}");
    assert!(
        page.contains(&format!(r#"<a href="/ui/rooms/{room}">lab</a>"#)),
        "{page}"
    );
    assert!(
        page.contains(r#"<span class="role">editor</span>"#),
        "{page}"
    );
}

#[tokio::test]
async fn a_mailed_group_link_keeps_the_token_out_of_the_path() {
    let fixture = Fixture::start_with_mail().await;
    let owner = fixture.verified(OWNER).await;
    let token = fixture.token(owner).await;
    let id = group_owned_by(&fixture, owner, "friends").await;

    let response = fixture
        .post(
            &format!("/ui/groups/{id}/invitations"),
            Some(&token),
            &form(&[("kind", "email"), ("email", GUEST)]),
        )
        .await;
    assert_eq!(location(&response), format!("/ui/groups/{id}"));

    let mail = fixture.wait_for_mail(GUEST).await;
    let mail = mail.replace("=\r\n", "").replace("=\n", "");
    assert!(mail.contains("/ui/groups/join#"), "{mail}");
    assert!(!mail.contains("/ui/groups/join/"), "{mail}");
}

#[tokio::test]
async fn a_token_in_the_path_is_not_a_page() {
    let fixture = Fixture::start().await;

    let response = fixture.get("/ui/groups/join/some-token", None).await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Removing a member answers a caller who may not act the same way whether the id names an
/// account or nothing at all.
#[tokio::test]
async fn unknown_and_existing_ids_answer_alike() {
    let fixture = Fixture::start().await;
    let owner = fixture.verified(OWNER).await;
    let known = fixture.verified("known@example.org").await;
    let stranger = fixture.verified("stranger@example.org").await;
    let id = group_owned_by(&fixture, owner, "friends").await;
    let plain = member_of(&fixture, &id, MEMBER).await;
    let missing = Uuid::new_v4();

    for caller in [owner, stranger, plain] {
        let token = fixture.token(caller).await;
        let existing = removal_answer(&fixture, &token, &id, known).await;
        let unknown = removal_answer(&fixture, &token, &id, missing).await;
        assert_eq!(
            existing, unknown,
            "the removal answer betrays whether the member id exists"
        );
    }
}

async fn removal_answer(
    fixture: &Fixture,
    token: &str,
    group_id: &str,
    member: Uuid,
) -> (StatusCode, String, Option<String>) {
    let response = fixture
        .post(
            &format!("/ui/groups/{group_id}/members/{member}/remove"),
            Some(token),
            "",
        )
        .await;
    (
        response.status(),
        location(&response),
        flash_cookie(&response),
    )
}

/// The group's email field suggests the people the owner shares a room with, not its members.
#[tokio::test]
async fn the_email_field_suggests_people_the_owner_works_with() {
    let fixture = Fixture::start().await;
    let owner = fixture.verified(OWNER).await;
    let group = group_owned_by(&fixture, owner, "friends").await;
    member_of(&fixture, &group, MEMBER).await;
    let room = lifecycle::create_room(&fixture.state, owner, "lab")
        .await
        .unwrap()
        .id;
    for email in [MEMBER, OTHER] {
        let id = db::user_by_email(&fixture.state.pool, email).await.unwrap();
        let id = match id {
            Some(user) => user.id,
            None => fixture.verified(email).await,
        };
        db::add_member(&fixture.state.pool, &room, id, Role::Editor)
            .await
            .unwrap();
    }
    fixture.verified(GUEST).await;
    let token = fixture.token(owner).await;

    let page = body_text(
        fixture
            .get(&format!("/ui/groups/{group}"), Some(&token))
            .await,
    )
    .await;

    assert!(page.contains(r#"list="known-people""#), "{page}");
    assert!(
        page.contains(&format!(r#"<option value="{OTHER}">"#)),
        "{page}"
    );
    assert!(
        !page.contains(&format!(r#"<option value="{MEMBER}">"#)),
        "{page}"
    );
    assert!(
        !page.contains(&format!(r#"<option value="{GUEST}">"#)),
        "{page}"
    );
    assert!(
        !page.contains(&format!(r#"<option value="{OWNER}">"#)),
        "{page}"
    );
}
