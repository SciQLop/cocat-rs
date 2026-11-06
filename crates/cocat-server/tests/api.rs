use std::fs;
use std::time::SystemTime;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use cocat_core::role::Role;
use cocat_server::room::RoomManager;
use cocat_server::{AppState, auth, db, links::Links, ratelimit::LoginLimit, ws};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    state: AppState,
    _dir: TempDir,
}

impl Fixture {
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
        };
        Self { state, _dir: dir }
    }

    async fn user(&self, email: &str) -> Uuid {
        db::create_user(&self.state.pool, email, "hunter2", false, true)
            .await
            .unwrap()
    }

    fn token(&self, user: Uuid) -> String {
        auth::mint_token(user, &self.state.secret, SystemTime::now()).unwrap()
    }

    fn router(&self) -> Router {
        ws::router(self.state.clone())
    }

    async fn call(&self, request: Request<Body>) -> (StatusCode, Value) {
        let response = self.router().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("the response is not JSON")
        };
        (status, body)
    }

    fn request(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<String>,
    ) -> Request<Body> {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            request = request.header(header::COOKIE, format!("fastapiusersauth={token}"));
        }
        let body = match body {
            Some(body) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(body)
            }
            None => Body::empty(),
        };
        request.body(body).unwrap()
    }

    async fn get(&self, path: &str, token: Option<&str>) -> (StatusCode, Value) {
        self.call(self.request(Method::GET, path, token, None))
            .await
    }

    async fn post(&self, path: &str, token: Option<&str>, body: Value) -> (StatusCode, Value) {
        self.post_raw(path, token, &body.to_string()).await
    }

    async fn post_raw(&self, path: &str, token: Option<&str>, body: &str) -> (StatusCode, Value) {
        self.call(self.request(Method::POST, path, token, Some(body.to_owned())))
            .await
    }

    async fn patch(&self, path: &str, token: Option<&str>, body: Value) -> (StatusCode, Value) {
        self.call(self.request(Method::PATCH, path, token, Some(body.to_string())))
            .await
    }

    async fn delete(&self, path: &str, token: Option<&str>) -> (StatusCode, Value) {
        self.call(self.request(Method::DELETE, path, token, None))
            .await
    }

    async fn room(&self, owner: Uuid, room_id: &str, name: &str) {
        db::create_room(&self.state.pool, owner, room_id, name)
            .await
            .unwrap();
    }

    async fn member(&self, room_id: &str, user: Uuid, role: Role) -> Uuid {
        db::add_member(&self.state.pool, room_id, user, role)
            .await
            .unwrap();
        user
    }
}

#[tokio::test]
async fn me_without_a_cookie_is_401() {
    let fixture = Fixture::start().await;

    let (status, body) = fixture.get("/api/me", None).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, json!({"detail": "Unauthorized"}));
}

#[tokio::test]
async fn me_returns_the_user() {
    let fixture = Fixture::start().await;
    let user = fixture.user("owner@example.org").await;

    let (status, body) = fixture.get("/api/me", Some(&fixture.token(user))).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], user.to_string());
    assert_eq!(body["email"], "owner@example.org");
    assert_eq!(body["is_active"], true);
    assert_eq!(body["is_superuser"], false);
    assert_eq!(body["is_verified"], true);
}

#[tokio::test]
async fn legacy_rooms_lists_room_ids() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room(owner, "zeta", "Zeta").await;
    fixture.room(owner, "alpha", "Alpha").await;

    let (status, body) = fixture.get("/rooms", Some(&fixture.token(owner))).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"rooms": ["alpha", "zeta"]}));
}

#[tokio::test]
async fn legacy_room_users_needs_membership() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let member = fixture.user("member@example.org").await;
    let outsider = fixture.user("outsider@example.org").await;
    fixture.room(owner, "room-1", "Room").await;
    fixture.member("room-1", member, Role::Viewer).await;

    let (denied, _) = fixture
        .get("/room/room-1/users", Some(&fixture.token(outsider)))
        .await;
    let (status, body) = fixture
        .get("/room/room-1/users", Some(&fixture.token(member)))
        .await;

    assert_eq!(denied, StatusCode::NOT_FOUND);
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"users": ["member@example.org", "owner@example.org"]})
    );
}

#[tokio::test]
async fn create_list_rename_delete_room() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let token = fixture.token(owner);

    let (status, room) = fixture
        .post("/api/rooms", Some(&token), json!({"name": "Lab"}))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(room["name"], "Lab");
    assert_eq!(room["role"], "owner");
    let room_id = room["id"].as_str().expect("no room id").to_owned();

    let (status, listed) = fixture.get("/api/rooms", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed, json!([room]));

    let (status, renamed) = fixture
        .patch(
            &format!("/api/rooms/{room_id}"),
            Some(&token),
            json!({"name": "Renamed"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        renamed,
        json!({"id": room_id, "name": "Renamed", "role": "owner"})
    );

    let (status, _) = fixture
        .delete(&format!("/api/rooms/{room_id}"), Some(&token))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, listed) = fixture.get("/api/rooms", Some(&token)).await;
    assert_eq!(listed, json!([]));
}

#[tokio::test]
async fn member_management() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let editor = fixture.user("editor@example.org").await;
    let token = fixture.token(owner);
    fixture.room(owner, "room-members", "Members").await;
    fixture.member("room-members", editor, Role::Editor).await;

    let (status, members) = fixture
        .get("/api/rooms/room-members/members", Some(&token))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        members,
        json!([
            {"user_id": editor.to_string(), "email": "editor@example.org", "role": "editor"},
            {"user_id": owner.to_string(), "email": "owner@example.org", "role": "owner"},
        ])
    );

    let (status, _) = fixture
        .patch(
            &format!("/api/rooms/room-members/members/{editor}"),
            Some(&token),
            json!({"role": "editor"}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = fixture
        .post(
            "/api/rooms/room-members/owner",
            Some(&token),
            json!({"user_id": editor}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = fixture
        .delete(
            &format!("/api/rooms/room-members/members/{owner}"),
            Some(&fixture.token(editor)),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        db::role_in(&fixture.state.pool, "room-members", owner)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn errors_map_to_status() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let editor = fixture.user("editor@example.org").await;
    let stranger = fixture.user("stranger@example.org").await;
    fixture.room(owner, "room-status", "Status").await;
    fixture.member("room-status", editor, Role::Editor).await;

    let (renamed, _) = fixture
        .patch(
            "/api/rooms/room-status",
            Some(&fixture.token(editor)),
            json!({"name": "X"}),
        )
        .await;
    assert_eq!(renamed, StatusCode::FORBIDDEN);

    let (stranger_denied, _) = fixture
        .get(
            "/api/rooms/room-status/members",
            Some(&fixture.token(stranger)),
        )
        .await;
    let (missing_denied, _) = fixture
        .get("/api/rooms/missing/members", Some(&fixture.token(owner)))
        .await;
    assert_eq!(stranger_denied, StatusCode::NOT_FOUND);
    assert_eq!(missing_denied, StatusCode::NOT_FOUND);

    let (leaving, _) = fixture
        .delete(
            &format!("/api/rooms/room-status/members/{owner}"),
            Some(&fixture.token(owner)),
        )
        .await;
    assert_eq!(leaving, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_blank_room_name_is_a_bad_request() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;

    let (status, body) = fixture
        .post(
            "/api/rooms",
            Some(&fixture.token(owner)),
            json!({"name": "   "}),
        )
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, json!({"detail": "Enter a room name"}));
}

#[tokio::test]
async fn a_room_name_with_control_characters_is_refused() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room(owner, "room-named", "Named").await;

    let (created, _) = fixture
        .post(
            "/api/rooms",
            Some(&fixture.token(owner)),
            json!({"name": "a\tb"}),
        )
        .await;
    let (renamed, body) = fixture
        .patch(
            "/api/rooms/room-named",
            Some(&fixture.token(owner)),
            json!({"name": "a\u{7f}b"}),
        )
        .await;

    assert_eq!(created, StatusCode::BAD_REQUEST);
    assert_eq!(renamed, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({"detail": "Enter a room name without control characters"})
    );
}

#[tokio::test]
async fn a_too_long_room_name_is_422() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let token = fixture.token(owner);

    let (empty, body) = fixture
        .post("/api/rooms", Some(&token), json!({"name": ""}))
        .await;
    let (long, _) = fixture
        .post("/api/rooms", Some(&token), json!({"name": "a".repeat(201)}))
        .await;

    assert_eq!(empty, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["detail"].is_string());
    assert_eq!(long, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_malformed_json_body_is_422() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let token = fixture.token(owner);
    fixture.room(owner, "room-body", "Body").await;
    fixture
        .member(
            "room-body",
            fixture.user("editor@example.org").await,
            Role::Editor,
        )
        .await;

    let (syntax, syntax_body) = fixture.post_raw("/api/rooms", Some(&token), "{").await;
    let (types, types_body) = fixture
        .post_raw("/api/rooms", Some(&token), r#"{"name": 42}"#)
        .await;
    let (role, _) = fixture
        .patch(
            "/api/rooms/room-body/members/00000000-0000-0000-0000-000000000000",
            Some(&token),
            json!({"role": "superuser"}),
        )
        .await;

    assert_eq!(syntax, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(syntax_body["detail"].is_string());
    assert_eq!(types, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(types_body["detail"].is_string());
    assert_eq!(role, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_bad_path_id_is_422() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let token = fixture.token(owner);
    fixture.room(owner, "room-path", "Path").await;

    let (status, body) = fixture
        .patch(
            "/api/rooms/room-path/members/not-a-uuid",
            Some(&token),
            json!({"role": "editor"}),
        )
        .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["detail"].is_string());
}

#[tokio::test]
async fn the_owner_can_assign_the_viewer_role() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let member = fixture.user("member@example.org").await;
    fixture.room(owner, "room-role", "Role").await;
    fixture.member("room-role", member, Role::Editor).await;

    let (status, _) = fixture
        .patch(
            &format!("/api/rooms/room-role/members/{member}"),
            Some(&fixture.token(owner)),
            json!({"role": "viewer"}),
        )
        .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        db::role_in(&fixture.state.pool, "room-role", member)
            .await
            .unwrap(),
        Some(Role::Viewer)
    );
}

#[tokio::test]
async fn the_owner_role_is_not_assignable_over_http() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let member = fixture.user("member@example.org").await;
    fixture.room(owner, "room-owner", "Owner").await;
    fixture.member("room-owner", member, Role::Editor).await;

    let (status, body) = fixture
        .patch(
            &format!("/api/rooms/room-owner/members/{member}"),
            Some(&fixture.token(owner)),
            json!({"role": "owner"}),
        )
        .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["detail"].is_string());
    assert_eq!(
        db::role_in(&fixture.state.pool, "room-owner", member)
            .await
            .unwrap(),
        Some(Role::Editor)
    );
}

#[tokio::test]
async fn invite_link_flow() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let guest = fixture.user("guest@example.org").await;
    let token = fixture.token(owner);
    let (status, room) = fixture
        .post("/api/rooms", Some(&token), json!({"name": "Lab"}))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let room_id = room["id"].as_str().expect("no room id").to_owned();

    let (created, body) = fixture
        .post(
            &format!("/api/rooms/{room_id}/invitations"),
            Some(&token),
            json!({}),
        )
        .await;
    assert_eq!(created, StatusCode::CREATED);
    assert!(body["id"].is_i64());
    let invitation_token = body["token"].as_str().expect("no token").to_owned();

    let (status, preview) = fixture
        .post(
            "/api/invitations/preview",
            None,
            json!({"token": invitation_token}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(preview["room_id"], room_id);
    assert_eq!(preview["room_name"], "Lab");
    assert_eq!(preview["role"], "editor");
    assert_eq!(preview["inviter_email"], "owner@example.org");
    assert!(preview["expires_at"].as_str().unwrap().ends_with('Z'));

    let (accepted, joined) = fixture
        .post(
            "/api/invitations/accept",
            Some(&fixture.token(guest)),
            json!({"token": invitation_token}),
        )
        .await;
    assert_eq!(accepted, StatusCode::OK);
    assert_eq!(
        joined,
        json!({"id": room_id, "name": "Lab", "role": "editor"})
    );

    let (_, members) = fixture
        .get(&format!("/api/rooms/{room_id}/members"), Some(&token))
        .await;
    let guest_member = members
        .as_array()
        .expect("the member list is not an array")
        .iter()
        .find(|member| member["email"] == "guest@example.org")
        .expect("the guest is not a member");
    assert_eq!(guest_member["role"], "editor");
}

#[tokio::test]
async fn invitations_are_listed_and_revoked() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let token = fixture.token(owner);
    fixture.room(owner, "room-invites", "Invites").await;

    let (_, created) = fixture
        .post(
            "/api/rooms/room-invites/invitations",
            Some(&token),
            json!({"role": "viewer", "expires_in_days": 7, "max_uses": 3}),
        )
        .await;
    let invitation_id = created["id"].as_i64().expect("no invitation id");

    let (status, listed) = fixture
        .get("/api/rooms/room-invites/invitations", Some(&token))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed[0]["id"], invitation_id);
    assert_eq!(listed[0]["email"], Value::Null);
    assert_eq!(listed[0]["role"], "viewer");
    assert_eq!(listed[0]["uses"], 0);
    assert_eq!(listed[0]["max_uses"], 3);

    let (status, _) = fixture
        .delete(
            &format!("/api/rooms/room-invites/invitations/{invitation_id}"),
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, listed) = fixture
        .get("/api/rooms/room-invites/invitations", Some(&token))
        .await;
    assert_eq!(listed, json!([]));
}

/// Python's `InvitationCreate.email: EmailStr | None` refuses these in the schema (422),
/// before `actions._address` ever runs; only a placeholder is left for the 400 below.
#[tokio::test]
async fn a_malformed_invitation_address_is_422() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let token = fixture.token(owner);
    fixture.room(owner, "room-bad-mail", "Bad mail").await;

    for address in ["", "   ", "not-an-address", "a@b"] {
        let (status, body) = fixture
            .post(
                "/api/rooms/room-bad-mail/invitations",
                Some(&token),
                json!({"email": address}),
            )
            .await;

        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{address:?} answered {body}"
        );
        assert!(body["detail"].is_string(), "{body}");
    }
}

#[tokio::test]
async fn the_error_messages_are_pythons() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let owner_token = fixture.token(owner);
    fixture.room(owner, "room-words", "Words").await;
    let viewer = fixture.user("viewer@example.org").await;
    fixture.member("room-words", viewer, Role::Viewer).await;
    let viewer_token = fixture.token(viewer);
    let stranger = Uuid::new_v4();

    let (status, body) = fixture
        .patch(
            "/api/rooms/room-words",
            Some(&viewer_token),
            json!({"name": "Renamed"}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body,
        json!({"detail": "Room room-words requires the owner role"})
    );

    let (status, body) = fixture
        .patch(
            &format!("/api/rooms/room-words/members/{stranger}"),
            Some(&owner_token),
            json!({"role": "editor"}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body,
        json!({"detail": format!("User {stranger} is not a member of room room-words")})
    );

    let (status, body) = fixture
        .get("/api/rooms/room-missing/members", Some(&owner_token))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({"detail": "Room room-missing not found"}));

    let (_, created) = fixture
        .post(
            "/api/rooms/room-words/invitations",
            Some(&owner_token),
            json!({"email": "guest@example.org"}),
        )
        .await;
    let token = created["token"].as_str().expect("no token").to_owned();
    let guest_token = fixture.token(fixture.user("stranger@example.org").await);
    let (status, body) = fixture
        .post(
            "/api/invitations/accept",
            Some(&guest_token),
            json!({"token": token}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body,
        json!({"detail": "This invitation is for another email address"})
    );
}

#[tokio::test]
async fn a_placeholder_address_is_refused() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let token = fixture.token(owner);
    fixture.room(owner, "room-mail", "Mail").await;

    let (status, body) = fixture
        .post(
            "/api/rooms/room-mail/invitations",
            Some(&token),
            json!({"email": "someone@orcid.placeholder.cocat.org"}),
        )
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, json!({"detail": "Enter a valid email address"}));
}

#[tokio::test]
async fn an_invitation_cannot_grant_owner_over_http() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    let token = fixture.token(owner);
    fixture.room(owner, "room-grant", "Grant").await;

    let (status, body) = fixture
        .post(
            "/api/rooms/room-grant/invitations",
            Some(&token),
            json!({"role": "owner"}),
        )
        .await;
    let (out_of_range, _) = fixture
        .post(
            "/api/rooms/room-grant/invitations",
            Some(&token),
            json!({"expires_in_days": 91}),
        )
        .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["detail"].is_string());
    assert_eq!(out_of_range, StatusCode::UNPROCESSABLE_ENTITY);
}
