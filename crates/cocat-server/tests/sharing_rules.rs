//! Who may share a project: its owner, and its editors while the owner lets them.

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use cocat_core::role::Role;
use cocat_server::room::RoomManager;
use cocat_server::{
    AppState, auth, db, groups, invitations, links::Links, ratelimit::LoginLimit, ws,
};
use serde_json::{Value, json};
use std::fs;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

const ROOM: &str = "lab";

/// A project with an owner, an editor, a viewer and a pending invitation of the owner's.
struct World {
    state: AppState,
    owner: Uuid,
    editor: Uuid,
    viewer: Uuid,
    invitation: i64,
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
        let editor = user(&state, "editor@example.org").await;
        let viewer = user(&state, "viewer@example.org").await;
        db::create_room(&state.pool, owner, ROOM, "Lab", db::ProjectKind::Catalogues)
            .await
            .unwrap();
        db::add_member(&state.pool, ROOM, editor, Role::Editor)
            .await
            .unwrap();
        db::add_member(&state.pool, ROOM, viewer, Role::Viewer)
            .await
            .unwrap();
        let (invitation, _) =
            invitations::create(&state, ROOM, owner, Role::Viewer, None, 14, None)
                .await
                .unwrap();
        Self {
            state,
            owner,
            editor,
            viewer,
            invitation,
            _dir: dir,
        }
    }

    async fn close_sharing(&self) {
        let (status, _) = self
            .call(
                self.owner,
                Method::PUT,
                "/api/rooms/lab/editors-can-share",
                Some(json!({"allowed": false})),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
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

    async fn status(&self, caller: Uuid, method: Method, path: &str, body: Option<Value>) -> u16 {
        self.call(caller, method, path, body).await.0.as_u16()
    }

    async fn role_of(&self, user: Uuid) -> Option<Role> {
        db::role_in(&self.state.pool, ROOM, user).await.unwrap()
    }

    async fn listed_setting(&self, caller: Uuid) -> Value {
        let (_, listed) = self.call(caller, Method::GET, "/api/rooms", None).await;
        listed[0]["editors_can_share"].clone()
    }
}

async fn user(state: &AppState, email: &str) -> Uuid {
    let id = Uuid::new_v4();
    db::insert_user(&state.pool, id, email, "not-a-hash", false, true)
        .await
        .unwrap();
    id
}

#[tokio::test]
async fn a_project_lets_its_editors_share_until_the_owner_says_otherwise() {
    let world = World::start().await;

    assert_eq!(world.listed_setting(world.editor).await, json!(true));
    world.close_sharing().await;
    assert_eq!(world.listed_setting(world.editor).await, json!(false));
    assert_eq!(world.listed_setting(world.owner).await, json!(false));
}

#[tokio::test]
async fn an_editor_shares_like_the_owner() {
    let world = World::start().await;
    let team = groups::create(&world.state, world.editor, "team")
        .await
        .unwrap();
    let invite = Some(json!({"email": "new@example.org", "role": "viewer"}));

    let invited = world
        .status(
            world.editor,
            Method::POST,
            "/api/rooms/lab/invitations",
            invite,
        )
        .await;
    let (listed, pending) = world
        .call(
            world.editor,
            Method::GET,
            "/api/rooms/lab/invitations",
            None,
        )
        .await;
    let suggested = world
        .status(
            world.editor,
            Method::GET,
            "/api/rooms/lab/suggestions",
            None,
        )
        .await;
    let revoked = world
        .status(
            world.editor,
            Method::DELETE,
            &format!("/api/rooms/lab/invitations/{}", world.invitation),
            None,
        )
        .await;
    let promoted = world
        .status(
            world.editor,
            Method::PATCH,
            &format!("/api/rooms/lab/members/{}", world.viewer),
            Some(json!({"role": "editor"})),
        )
        .await;
    let granted = world
        .status(
            world.editor,
            Method::PUT,
            &format!("/api/rooms/lab/groups/{}", team.id),
            Some(json!({"role": "viewer"})),
        )
        .await;
    let ungranted = world
        .status(
            world.editor,
            Method::DELETE,
            &format!("/api/rooms/lab/groups/{}", team.id),
            None,
        )
        .await;
    let removed = world
        .status(
            world.editor,
            Method::DELETE,
            &format!("/api/rooms/lab/members/{}", world.viewer),
            None,
        )
        .await;

    assert_eq!(invited, 201);
    assert_eq!(listed, StatusCode::OK);
    assert_eq!(pending.as_array().map(Vec::len), Some(2), "{pending}");
    assert_eq!(suggested, 200);
    assert_eq!(revoked, 204, "any invitation, not only their own");
    assert_eq!(promoted, 204);
    assert_eq!(granted, 200);
    assert_eq!(ungranted, 204);
    assert_eq!(removed, 204);
    assert_eq!(world.role_of(world.viewer).await, None);
}

#[tokio::test]
async fn an_editor_grants_only_a_group_they_belong_to() {
    let world = World::start().await;
    let theirs = groups::create(&world.state, world.owner, "owners")
        .await
        .unwrap();

    let granted = world
        .status(
            world.editor,
            Method::PUT,
            &format!("/api/rooms/lab/groups/{}", theirs.id),
            Some(json!({"role": "viewer"})),
        )
        .await;

    assert_eq!(granted, 404);
}

#[tokio::test]
async fn with_sharing_closed_an_editor_shares_nothing() {
    let world = World::start().await;
    let team = groups::create(&world.state, world.editor, "team")
        .await
        .unwrap();
    world.close_sharing().await;
    let member = format!("/api/rooms/lab/members/{}", world.viewer);
    let group = format!("/api/rooms/lab/groups/{}", team.id);
    let invitation = format!("/api/rooms/lab/invitations/{}", world.invitation);
    let calls = [
        (
            Method::POST,
            "/api/rooms/lab/invitations",
            Some(json!({"role": "viewer"})),
        ),
        (Method::GET, "/api/rooms/lab/invitations", None),
        (Method::DELETE, invitation.as_str(), None),
        (Method::GET, "/api/rooms/lab/suggestions", None),
        (
            Method::PATCH,
            member.as_str(),
            Some(json!({"role": "editor"})),
        ),
        (Method::DELETE, member.as_str(), None),
        (Method::PUT, group.as_str(), Some(json!({"role": "viewer"}))),
    ];

    for (method, path, body) in calls {
        let (status, refusal) = world.call(world.editor, method.clone(), path, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
        assert_eq!(refusal["detail"], "Only the owner can share project lab");
    }
    assert_eq!(world.role_of(world.viewer).await, Some(Role::Viewer));
}

#[tokio::test]
async fn with_sharing_closed_an_editor_still_sees_renames_and_leaves() {
    let world = World::start().await;
    world.close_sharing().await;

    let (seen, members) = world
        .call(world.editor, Method::GET, "/api/rooms/lab/members", None)
        .await;
    let (renamed, room) = world
        .call(
            world.editor,
            Method::PATCH,
            "/api/rooms/lab",
            Some(json!({"name": "Bow shocks"})),
        )
        .await;
    let left = world
        .status(
            world.editor,
            Method::DELETE,
            &format!("/api/rooms/lab/members/{}", world.editor),
            None,
        )
        .await;

    assert_eq!(seen, StatusCode::OK);
    assert_eq!(members.as_array().map(Vec::len), Some(3));
    assert_eq!(renamed, StatusCode::OK);
    assert_eq!(
        room,
        json!({"id": "lab", "name": "Bow shocks", "role": "editor"})
    );
    assert_eq!(left, 204);
}

#[tokio::test]
async fn a_viewer_shares_nothing_whatever_the_setting() {
    let world = World::start().await;
    let invite = Some(json!({"role": "viewer"}));

    let open = world
        .status(
            world.viewer,
            Method::POST,
            "/api/rooms/lab/invitations",
            invite.clone(),
        )
        .await;
    world.close_sharing().await;
    let closed = world
        .status(
            world.viewer,
            Method::POST,
            "/api/rooms/lab/invitations",
            invite,
        )
        .await;
    let renamed = world
        .status(
            world.viewer,
            Method::PATCH,
            "/api/rooms/lab",
            Some(json!({"name": "Mine"})),
        )
        .await;

    assert_eq!((open, closed, renamed), (403, 403, 403));
}

#[tokio::test]
async fn an_editor_never_touches_the_ownership() {
    let world = World::start().await;
    let team = groups::create(&world.state, world.editor, "team")
        .await
        .unwrap();
    let owner = format!("/api/rooms/lab/members/{}", world.owner);
    let own = format!("/api/rooms/lab/members/{}", world.editor);
    let viewer = format!("/api/rooms/lab/members/{}", world.viewer);
    let group = format!("/api/rooms/lab/groups/{}", team.id);
    let transfer = Some(json!({"user_id": world.editor}));
    let calls = [
        (Method::DELETE, "/api/rooms/lab", None, 403),
        (Method::POST, "/api/rooms/lab/owner", transfer, 403),
        (
            Method::PUT,
            "/api/rooms/lab/editors-can-share",
            Some(json!({"allowed": false})),
            403,
        ),
        (
            Method::PATCH,
            owner.as_str(),
            Some(json!({"role": "viewer"})),
            409,
        ),
        (Method::DELETE, owner.as_str(), None, 409),
        (
            Method::PATCH,
            viewer.as_str(),
            Some(json!({"role": "owner"})),
            422,
        ),
        (
            Method::PATCH,
            own.as_str(),
            Some(json!({"role": "owner"})),
            422,
        ),
        (
            Method::POST,
            "/api/rooms/lab/invitations",
            Some(json!({"role": "owner"})),
            422,
        ),
        (
            Method::PUT,
            group.as_str(),
            Some(json!({"role": "owner"})),
            422,
        ),
    ];

    for (method, path, body, wanted) in calls {
        let status = world.status(world.editor, method.clone(), path, body).await;
        assert_eq!(status, wanted, "{method} {path}");
    }
    assert_eq!(world.role_of(world.owner).await, Some(Role::Owner));
    assert_eq!(world.role_of(world.editor).await, Some(Role::Editor));
    assert_eq!(world.listed_setting(world.owner).await, json!(true));
}

#[tokio::test]
async fn an_editor_steps_down_but_never_back_up() {
    let world = World::start().await;
    let own = format!("/api/rooms/lab/members/{}", world.editor);

    let down = world
        .status(
            world.editor,
            Method::PATCH,
            &own,
            Some(json!({"role": "viewer"})),
        )
        .await;
    let up = world
        .status(
            world.editor,
            Method::PATCH,
            &own,
            Some(json!({"role": "editor"})),
        )
        .await;

    assert_eq!((down, up), (204, 403));
    assert_eq!(world.role_of(world.editor).await, Some(Role::Viewer));
}

#[tokio::test]
async fn only_the_owner_changes_the_setting() {
    let world = World::start().await;
    let stranger = user(&world.state, "stranger@example.org").await;
    let path = "/api/rooms/lab/editors-can-share";
    let closed = Some(json!({"allowed": false}));

    let viewer = world
        .status(world.viewer, Method::PUT, path, closed.clone())
        .await;
    let outsider = world.status(stranger, Method::PUT, path, closed).await;
    let garbled = world
        .status(
            world.owner,
            Method::PUT,
            path,
            Some(json!({"allowed": "no"})),
        )
        .await;
    let reopened = world
        .status(
            world.owner,
            Method::PUT,
            path,
            Some(json!({"allowed": true})),
        )
        .await;

    assert_eq!((viewer, outsider, garbled, reopened), (403, 404, 422, 204));
    assert_eq!(world.listed_setting(world.owner).await, json!(true));
}

/// The SciQLop plugin shows the sharing controls to whoever may list the invitations, so the
/// listing answers exactly by the sharing rule.
#[tokio::test]
async fn listing_the_invitations_answers_by_the_sharing_rule() {
    let world = World::start().await;
    let stranger = user(&world.state, "stranger@example.org").await;
    let path = "/api/rooms/lab/invitations";
    let list = |caller| world.status(caller, Method::GET, path, None);

    let open = (
        list(world.owner).await,
        list(world.editor).await,
        list(world.viewer).await,
        list(stranger).await,
    );
    world.close_sharing().await;
    let closed = (
        list(world.owner).await,
        list(world.editor).await,
        list(world.viewer).await,
        list(stranger).await,
    );

    assert_eq!(open, (200, 200, 403, 404));
    assert_eq!(closed, (200, 403, 403, 404));
}
