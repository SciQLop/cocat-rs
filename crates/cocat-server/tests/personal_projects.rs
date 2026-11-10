//! An account's own project, "My catalogues", never gains people: no invitation, no new or
//! re-added member, no group, no new owner. People who were in it before stay.

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use cocat_core::role::Role;
use cocat_server::room::RoomManager;
use cocat_server::{AppState, auth, db, groups, links::Links, ratelimit::LoginLimit, ws};
use serde_json::{Value, json};
use std::fs;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

const PERSONAL: &str =
    "“My catalogues” is personal: copy catalogues into another project to share them.";
const LAB: &str = "lab";
const OLD_TOKEN: &str = "an-invitation-from-before-the-rule";

/// An owner with their own project and a shared one, a friend, and a group of the owner's
/// that holds the friend.
struct World {
    state: AppState,
    owner: Uuid,
    friend: Uuid,
    personal: String,
    team: String,
    _dir: TempDir,
}

impl World {
    async fn start() -> Self {
        let dir = TempDir::new().unwrap();
        let rooms = dir.path().join("rooms");
        fs::create_dir_all(&rooms).unwrap();
        let pool = db::open(&dir.path().join("test.db")).await.unwrap();
        let state = AppState {
            pool,
            secret: "test-secret".to_owned(),
            rooms: RoomManager::new(rooms, None),
            before_attach: None,
            login_limit: LoginLimit::default(),
            mailer: None,
            links: Links::default(),
            oidc: Vec::new(),
            web_dir: None,
            providers: Default::default(),
            http: Default::default(),
        };
        let owner = user(&state, "owner@example.org").await;
        let friend = user(&state, "friend@example.org").await;
        db::create_personal_room(&state.pool, owner).await.unwrap();
        let personal = db::personal_room(&state.pool, owner)
            .await
            .unwrap()
            .unwrap();
        db::create_room(&state.pool, owner, LAB, "Lab", db::ProjectKind::Catalogues)
            .await
            .unwrap();
        let team = groups::create(&state, owner, "team").await.unwrap().id;
        Self {
            state,
            owner,
            friend,
            personal,
            team,
            _dir: dir,
        }
    }

    async fn call(
        &self,
        caller: Uuid,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let token = auth::start_session(&self.state.pool, caller).await.unwrap();
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::COOKIE, format!("fastapiusersauth={token}"));
        let body = match body {
            Some(body) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        let response = ws::router(self.state.clone())
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    /// An invitation to the owner's project made before the rule, by writing it directly.
    async fn old_invitation(&self, email: Option<&str>) -> i64 {
        let hash = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(OLD_TOKEN));
        sqlx::query(
            "INSERT INTO invitation (token_hash, room_id, role, email, created_by, expires_at)
             VALUES (?, ?, 'editor', ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+1 days'))",
        )
        .bind(hash)
        .bind(&self.personal)
        .bind(email)
        .bind(self.owner.to_string())
        .execute(&self.state.pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn role_of(&self, room: &str, user: Uuid) -> Option<Role> {
        db::role_in(&self.state.pool, room, user).await.unwrap()
    }

    async fn granted_groups(&self, room: &str) -> usize {
        groups::room_groups(&self.state, room, self.owner)
            .await
            .unwrap()
            .len()
    }
}

async fn user(state: &AppState, email: &str) -> Uuid {
    let id = Uuid::new_v4();
    db::insert_user(&state.pool, id, email, "not-a-hash", false, true)
        .await
        .unwrap();
    id
}

fn refused(answer: (StatusCode, Value)) {
    assert_eq!(answer.0, StatusCode::CONFLICT, "{}", answer.1);
    assert_eq!(answer.1["detail"], PERSONAL);
}

#[tokio::test]
async fn nobody_is_invited_to_a_personal_project() {
    let world = World::start().await;
    let path = format!("/api/rooms/{}/invitations", world.personal);

    for body in [
        json!({"role": "viewer"}),
        json!({"email": "x@example.org", "role": "editor"}),
    ] {
        refused(
            world
                .call(world.owner, Method::POST, &path, Some(body))
                .await,
        );
    }
    let (_, pending) = world.call(world.owner, Method::GET, &path, None).await;
    assert_eq!(pending, json!([]));
}

#[tokio::test]
async fn a_personal_project_with_a_file_list_still_has_nobody_invited() {
    let world = World::start().await;
    db::create_document(
        &world.state.pool,
        &world.personal,
        db::DocumentKind::Manifest,
        world.owner,
    )
    .await
    .unwrap();
    let path = format!("/api/rooms/{}/invitations", world.personal);

    refused(
        world
            .call(
                world.owner,
                Method::POST,
                &path,
                Some(json!({"role": "viewer"})),
            )
            .await,
    );
}

#[tokio::test]
async fn an_invitation_from_before_the_rule_adds_nobody() {
    let world = World::start().await;
    world.old_invitation(None).await;

    refused(
        world
            .call(
                world.friend,
                Method::POST,
                "/api/invitations/accept",
                Some(json!({"token": OLD_TOKEN})),
            )
            .await,
    );
    assert_eq!(world.role_of(&world.personal, world.friend).await, None);
}

#[tokio::test]
async fn an_addressed_invitation_from_before_the_rule_adds_nobody() {
    let world = World::start().await;
    let id = world.old_invitation(Some("friend@example.org")).await;

    refused(
        world
            .call(
                world.friend,
                Method::POST,
                &format!("/api/invitations/{id}/accept"),
                None,
            )
            .await,
    );
    assert_eq!(world.role_of(&world.personal, world.friend).await, None);
}

#[tokio::test]
async fn an_old_member_is_not_re_added_and_stays() {
    let world = World::start().await;
    db::add_member(
        &world.state.pool,
        &world.personal,
        world.friend,
        Role::Viewer,
    )
    .await
    .unwrap();
    world.old_invitation(None).await;

    refused(
        world
            .call(
                world.friend,
                Method::POST,
                "/api/invitations/accept",
                Some(json!({"token": OLD_TOKEN})),
            )
            .await,
    );
    assert_eq!(
        world.role_of(&world.personal, world.friend).await,
        Some(Role::Viewer)
    );
}

#[tokio::test]
async fn a_personal_project_is_not_granted_to_a_group() {
    let world = World::start().await;

    refused(
        world
            .call(
                world.owner,
                Method::PUT,
                &format!("/api/rooms/{}/groups/{}", world.personal, world.team),
                Some(json!({"role": "viewer"})),
            )
            .await,
    );
    assert_eq!(world.granted_groups(&world.personal).await, 0);
}

#[tokio::test]
async fn a_personal_project_is_not_handed_over() {
    let world = World::start().await;
    db::add_member(
        &world.state.pool,
        &world.personal,
        world.friend,
        Role::Editor,
    )
    .await
    .unwrap();

    refused(
        world
            .call(
                world.owner,
                Method::POST,
                &format!("/api/rooms/{}/owner", world.personal),
                Some(json!({"user_id": world.friend})),
            )
            .await,
    );
    assert_eq!(
        world.role_of(&world.personal, world.owner).await,
        Some(Role::Owner)
    );
    assert_eq!(
        world.role_of(&world.personal, world.friend).await,
        Some(Role::Editor)
    );
}

#[tokio::test]
async fn another_project_of_the_same_owner_still_shares() {
    let world = World::start().await;
    let invitations = format!("/api/rooms/{LAB}/invitations");

    let (invited, created) = world
        .call(
            world.owner,
            Method::POST,
            &invitations,
            Some(json!({"role": "editor"})),
        )
        .await;
    assert_eq!(invited, StatusCode::CREATED, "{created}");
    let (accepted, _) = world
        .call(
            world.friend,
            Method::POST,
            "/api/invitations/accept",
            Some(json!({"token": created["token"]})),
        )
        .await;
    let (granted, _) = world
        .call(
            world.owner,
            Method::PUT,
            &format!("/api/rooms/{LAB}/groups/{}", world.team),
            Some(json!({"role": "viewer"})),
        )
        .await;
    let (handed, _) = world
        .call(
            world.owner,
            Method::POST,
            &format!("/api/rooms/{LAB}/owner"),
            Some(json!({"user_id": world.friend})),
        )
        .await;

    assert_eq!(accepted, StatusCode::OK);
    assert_eq!(granted, StatusCode::OK);
    assert_eq!(handed, StatusCode::NO_CONTENT);
    assert_eq!(world.role_of(LAB, world.friend).await, Some(Role::Owner));
}
