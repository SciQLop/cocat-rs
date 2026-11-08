//! The administration pages: superusers only, and every form acts through the admin model.

mod ui_support;

use axum::http::StatusCode;
use cocat_server::db;
use ui_support::{Fixture, body_text, location};

async fn admin(fixture: &Fixture) -> String {
    let id = db::create_user(&fixture.state.pool, "admin@example.org", "pwd", true, true)
        .await
        .unwrap();
    fixture.token(id).await
}

const PAGES: [&str; 3] = ["/ui/admin", "/ui/admin/users", "/ui/admin/rooms"];

#[tokio::test]
async fn a_plain_member_gets_404_and_no_admin_link() {
    let fixture = Fixture::start().await;
    let member = fixture.verified("member@example.org").await;
    let token = fixture.token(member).await;

    for page in PAGES {
        let response = fixture.get(page, Some(&token)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{page}");
    }
    let rooms = body_text(fixture.get("/ui/rooms", Some(&token)).await).await;
    assert!(!rooms.contains("/ui/admin"), "{rooms}");
}

#[tokio::test]
async fn a_plain_member_cannot_act() {
    let fixture = Fixture::start().await;
    let member = fixture.verified("member@example.org").await;
    let victim = fixture.verified("victim@example.org").await;
    let token = fixture.token(member).await;

    let response = fixture
        .post(
            &format!("/ui/admin/users/{victim}/flag"),
            Some(&token),
            "flag=superuser&value=true",
        )
        .await;
    assert!(response.status().is_redirection());
    assert_eq!(location(&response), "/ui/rooms");
    let user = db::user_by_id(&fixture.state.pool, member)
        .await
        .unwrap()
        .unwrap();
    assert!(!user.is_superuser);
    let response = fixture
        .post(
            &format!("/ui/admin/users/{victim}/delete"),
            Some(&token),
            "",
        )
        .await;
    assert!(response.status().is_redirection());
    assert!(
        db::user_by_id(&fixture.state.pool, victim)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_superuser_sees_the_three_pages_and_the_link() {
    let fixture = Fixture::start().await;
    let token = admin(&fixture).await;
    fixture.verified("owner@example.org").await;

    for page in PAGES {
        let response = fixture.get(page, Some(&token)).await;
        assert_eq!(response.status(), StatusCode::OK, "{page}");
        let body = body_text(response).await;
        assert!(body.contains(r#"href="/ui/admin""#), "{page}: {body}");
        assert!(body.contains(">Projects</a>"), "{page}: {body}");
    }
    let users = body_text(fixture.get("/ui/admin/users", Some(&token)).await).await;
    assert!(users.contains("owner@example.org"), "{users}");
    let found = body_text(fixture.get("/ui/admin/users?q=OWNER", Some(&token)).await).await;
    assert!(found.contains("owner@example.org"), "{found}");
    assert!(!found.contains("victim@"), "{found}");
}

#[tokio::test]
async fn the_forms_change_accounts_and_rooms() {
    let fixture = Fixture::start().await;
    let token = admin(&fixture).await;
    let user = fixture.verified("user@example.org").await;
    let heir = fixture.verified("heir@example.org").await;
    db::create_room(&fixture.state.pool, user, "lab", "Lab")
        .await
        .unwrap();

    let response = fixture
        .post(
            &format!("/ui/admin/users/{user}/flag"),
            Some(&token),
            "flag=active&value=false",
        )
        .await;
    assert_eq!(location(&response), "/ui/admin/users");
    let account = db::user_by_id(&fixture.state.pool, user)
        .await
        .unwrap()
        .unwrap();
    assert!(!account.is_active);

    fixture
        .post(
            &format!("/ui/admin/users/{user}/quota"),
            Some(&token),
            "mb=250",
        )
        .await;
    assert_eq!(
        db::user_quota(&fixture.state.pool, user).await.unwrap(),
        Some(250)
    );
    fixture
        .post(
            &format!("/ui/admin/users/{user}/quota"),
            Some(&token),
            "mb=",
        )
        .await;
    assert_eq!(
        db::user_quota(&fixture.state.pool, user).await.unwrap(),
        None
    );

    fixture
        .post("/ui/admin/rooms/lab/limit", Some(&token), "mb=12")
        .await;
    assert_eq!(
        db::room_quota(&fixture.state.pool, "lab").await.unwrap(),
        Some(12)
    );

    let response = fixture
        .post(
            "/ui/admin/rooms/lab/owner",
            Some(&token),
            "email=heir%40example.org",
        )
        .await;
    assert_eq!(location(&response), "/ui/admin/rooms");
    assert_eq!(
        db::room_owner(&fixture.state.pool, "lab").await.unwrap(),
        Some(heir)
    );

    fixture
        .post("/ui/admin/rooms/lab/delete", Some(&token), "")
        .await;
    assert!(!db::room_exists(&fixture.state.pool, "lab").await.unwrap());
}

/// Rooms imported from Python keep ids with spaces; the forms must reach them.
#[tokio::test]
async fn a_room_id_with_a_space_round_trips() {
    let fixture = Fixture::start().await;
    let token = admin(&fixture).await;
    let owner = fixture.verified("owner@example.org").await;
    sqlx::query("INSERT INTO room (id, name) VALUES ('Bepi MSA', 'Bepi MSA')")
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    db::add_member(
        &fixture.state.pool,
        "Bepi MSA",
        owner,
        cocat_core::role::Role::Owner,
    )
    .await
    .unwrap();

    let page = body_text(fixture.get("/ui/admin/rooms", Some(&token)).await).await;
    assert!(page.contains("/ui/admin/rooms/Bepi%20MSA/limit"), "{page}");
    fixture
        .post("/ui/admin/rooms/Bepi%20MSA/limit", Some(&token), "mb=7")
        .await;
    assert_eq!(
        db::room_quota(&fixture.state.pool, "Bepi MSA")
            .await
            .unwrap(),
        Some(7)
    );
}

#[tokio::test]
async fn the_admin_is_told_they_cannot_demote_themselves() {
    let fixture = Fixture::start().await;
    let token = admin(&fixture).await;
    let me = db::user_by_email(&fixture.state.pool, "admin@example.org")
        .await
        .unwrap()
        .unwrap();

    let response = fixture
        .post(
            &format!("/ui/admin/users/{}/flag", me.id),
            Some(&token),
            "flag=superuser&value=false",
        )
        .await;

    assert_eq!(location(&response), "/ui/admin/users");
    let me = db::user_by_id(&fixture.state.pool, me.id)
        .await
        .unwrap()
        .unwrap();
    assert!(me.is_superuser);
}

/// A public catalogue has no owner to give: the rooms page tags it and offers no owner form,
/// and a hand-made form is refused with a message.
#[tokio::test]
async fn a_public_catalogue_shows_as_public_and_takes_no_owner() {
    let fixture = Fixture::start().await;
    let token = admin(&fixture).await;
    let heir = fixture.verified("heir@example.org").await;
    let room = db::PublicRoom {
        id: "public.shocks".to_owned(),
        name: "Shocks".to_owned(),
        folder: None,
        source: None,
        description: None,
        public_count: None,
        counted_file: None,
    };
    assert!(
        db::create_public_room(&fixture.state.pool, &room, heir)
            .await
            .unwrap()
    );

    let page = body_text(fixture.get("/ui/admin/rooms", Some(&token)).await).await;
    assert!(
        page.contains(r#"<span class="tag accent">public</span>"#),
        "{page}"
    );
    assert!(
        !page.contains("/ui/admin/rooms/public.shocks/owner"),
        "{page}"
    );

    let response = fixture
        .post(
            "/ui/admin/rooms/public.shocks/owner",
            Some(&token),
            "email=heir%40example.org",
        )
        .await;
    assert_eq!(location(&response), "/ui/admin/rooms");
    assert_eq!(
        db::room_owner(&fixture.state.pool, "public.shocks")
            .await
            .unwrap(),
        None
    );
}
