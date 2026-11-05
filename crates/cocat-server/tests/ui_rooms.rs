mod ui_support;

use axum::http::StatusCode;
use cocat_core::role::Role;
use cocat_server::{db, groups, lifecycle};
use ui_support::{Fixture, body_text, flash_cleared, flash_cookie, form, location};
use uuid::Uuid;

const MEMBER: &str = "member@example.org";
const OTHER: &str = "other@example.org";

async fn room_owned_by(fixture: &Fixture, owner: Uuid, name: &str) -> String {
    lifecycle::create_room(&fixture.state, owner, name)
        .await
        .unwrap()
        .id
}

/// The flash a POST leaves for the page it redirects to.
fn flashed(response: &axum::response::Response) -> String {
    flash_cookie(response).expect("the action flashes a message")
}

#[tokio::test]
async fn login_required() {
    let fixture = Fixture::start().await;

    let response = fixture.get("/ui/rooms", None).await;

    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(location(&response), "/ui/login?next=%2Fui%2Frooms");
}

#[tokio::test]
async fn lists_rooms_with_roles() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user);
    let id = room_owned_by(&fixture, user, "lab").await;

    let page = body_text(fixture.get("/ui/rooms", Some(&token)).await).await;

    assert!(
        page.contains(&format!(r#"href="/ui/rooms/{id}""#)),
        "{page}"
    );
    assert!(page.contains("lab"), "{page}");
    assert!(
        page.contains(r#"<span class="role">owner</span>"#),
        "{page}"
    );
    assert!(page.contains(">Rename<"), "{page}");
    assert!(page.contains(r#"data-confirm="Delete lab?""#), "{page}");
}

#[tokio::test]
async fn lists_rooms_reached_through_a_group() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let other = fixture.verified(OTHER).await;
    let token = fixture.token(user);
    let id = room_owned_by(&fixture, other, "shared-lab").await;
    let group = groups::create(&fixture.state, other, "team").await.unwrap();
    sqlx::query("INSERT INTO group_member (group_id, user_id, role) VALUES (?, ?, 'member')")
        .bind(&group.id)
        .bind(user.to_string())
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    groups::grant(&fixture.state, &id, &group.id, other, Role::Editor)
        .await
        .unwrap();

    let page = body_text(fixture.get("/ui/rooms", Some(&token)).await).await;

    assert!(page.contains("shared-lab"), "{page}");
    assert!(
        page.contains(r#"<span class="role">editor</span>"#),
        "{page}"
    );
}

#[tokio::test]
async fn create() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user);

    let response = fixture
        .post("/ui/rooms", Some(&token), &form(&[("name", "new-lab")]))
        .await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/ui/rooms");
    let rooms = db::list_user_rooms(&fixture.state.pool, user)
        .await
        .unwrap();
    assert_eq!(rooms.len(), 1, "{rooms:?}");
    assert_eq!(rooms[0].0.name, "new-lab");
}

#[tokio::test]
async fn create_requires_a_name() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user);

    let response = fixture
        .post("/ui/rooms", Some(&token), &form(&[("name", " ")]))
        .await;

    assert_eq!(location(&response), "/ui/rooms");
    let page = body_text(
        fixture
            .get_with("/ui/rooms", &token, &flashed(&response))
            .await,
    )
    .await;
    assert!(page.contains("Enter a room name"), "{page}");
    assert!(
        db::list_user_rooms(&fixture.state.pool, user)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn create_refuses_control_characters() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user);

    let response = fixture
        .post("/ui/rooms", Some(&token), &form(&[("name", "lab\u{1}")]))
        .await;

    let page = body_text(
        fixture
            .get_with("/ui/rooms", &token, &flashed(&response))
            .await,
    )
    .await;
    assert!(
        page.contains("Enter a room name without control characters"),
        "{page}"
    );
}

#[tokio::test]
async fn create_refuses_a_name_over_200_characters() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user);

    let long = "a".repeat(201);
    let response = fixture
        .post("/ui/rooms", Some(&token), &form(&[("name", &long)]))
        .await;

    let page = body_text(
        fixture
            .get_with("/ui/rooms", &token, &flashed(&response))
            .await,
    )
    .await;
    assert!(
        page.contains("Enter a name of at most 200 characters"),
        "{page}"
    );
}

#[tokio::test]
async fn rename() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user);
    let id = room_owned_by(&fixture, user, "lab").await;

    let response = fixture
        .post(
            &format!("/ui/rooms/{id}/rename"),
            Some(&token),
            &form(&[("name", "renamed")]),
        )
        .await;

    assert_eq!(location(&response), "/ui/rooms");
    let rooms = db::list_user_rooms(&fixture.state.pool, user)
        .await
        .unwrap();
    assert_eq!(rooms[0].0.name, "renamed");
}

#[tokio::test]
async fn delete() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user);
    let id = room_owned_by(&fixture, user, "lab").await;

    let response = fixture
        .post(&format!("/ui/rooms/{id}/delete"), Some(&token), "")
        .await;

    assert_eq!(location(&response), "/ui/rooms");
    assert!(
        db::list_user_rooms(&fixture.state.pool, user)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn leave() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let other = fixture.verified(OTHER).await;
    let token = fixture.token(user);
    let id = room_owned_by(&fixture, other, "lab").await;
    db::add_member(&fixture.state.pool, &id, user, Role::Editor)
        .await
        .unwrap();

    let response = fixture
        .post(&format!("/ui/rooms/{id}/leave"), Some(&token), "")
        .await;

    assert_eq!(location(&response), "/ui/rooms");
    assert!(
        db::list_user_rooms(&fixture.state.pool, user)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn owner_has_no_leave() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user);
    let id = room_owned_by(&fixture, user, "lab").await;

    let page = body_text(fixture.get("/ui/rooms", Some(&token)).await).await;

    assert!(!page.contains(&format!("/ui/rooms/{id}/leave")), "{page}");
    assert!(!page.contains(">Leave<"), "{page}");
}

#[tokio::test]
async fn action_refused_after_deactivation() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user);
    fixture.deactivate(MEMBER).await;

    let response = fixture
        .post("/ui/rooms", Some(&token), &form(&[("name", "lab")]))
        .await;

    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert!(location(&response).starts_with("/ui/login"), "{response:?}");
    assert!(
        db::list_user_rooms(&fixture.state.pool, user)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn flash_shown_once() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user);
    let response = fixture
        .post("/ui/rooms", Some(&token), &form(&[("name", "lab")]))
        .await;
    let cookie = flashed(&response);

    let page = fixture.get_with("/ui/rooms", &token, &cookie).await;

    assert!(flash_cleared(&page), "the page clears the flash cookie");
    let body = body_text(page).await;
    assert!(body.contains("Room created"), "{body}");

    let again = body_text(fixture.get("/ui/rooms", Some(&token)).await).await;
    assert!(!again.contains("Room created"), "{again}");
}

#[tokio::test]
async fn forged_flash_is_escaped() {
    let fixture = Fixture::start().await;
    let user = fixture.verified(MEMBER).await;
    let token = fixture.token(user);

    let page = body_text(
        fixture
            .get_with(
                "/ui/rooms",
                &token,
                "cocat_flash=%3Cscript%3Ealert(1)%3C/script%3E",
            )
            .await,
    )
    .await;

    assert!(!page.contains("<script>alert(1)</script>"), "{page}");
    assert!(page.contains("&#60;script&#62;"), "{page}");
}
