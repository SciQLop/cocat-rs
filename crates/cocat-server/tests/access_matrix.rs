//! Every route against every kind of caller.
//!
//! The table is the specification: a row says the least capable caller the route admits and,
//! by its style, what everyone below gets. `every_route_is_in_the_matrix` reads the router's
//! own source so a new route cannot be added without a row.

mod oidc_support;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use cocat_core::role::Role;
use cocat_server::room::RoomManager;
use cocat_server::{
    AppState, auth, db, groups, invitations, lifecycle, links::Links, oidc, ratelimit::LoginLimit,
    ws,
};
use std::fs;
use std::path::Path;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "hunter2";
const PEER: &str = "203.0.113.7:41000";

/// What a caller is allowed to be. The ladder is capability, not identity: a group owner
/// reaches the room through the group's grant, so for room routes they sit with the editor.
const CALLERS: [Kind; 11] = [
    Kind::Anonymous,
    Kind::Unverified,
    Kind::Deactivated,
    Kind::Stranger,
    Kind::Viewer,
    Kind::Editor,
    Kind::Owner,
    Kind::GroupViewer,
    Kind::GroupEditor,
    Kind::GroupOwner,
    Kind::Superuser,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Anonymous,
    Unverified,
    Deactivated,
    Stranger,
    Viewer,
    Editor,
    Owner,
    GroupViewer,
    GroupEditor,
    GroupOwner,
    Superuser,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Guard {
    Public,
    SignedIn,
    RoomViewer,
    RoomEditor,
    RoomOwner,
    GroupMember,
    GroupOwner,
    Superuser,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Style {
    /// JSON route: 401, 403 or 404 when refused, never 401/403/404 when admitted.
    Api,
    /// The websocket handshake: 403 when refused.
    Ws,
    /// A rendered page: 200 when admitted, 404 or a login redirect when refused.
    UiPage,
    /// A form post: a redirect or a re-rendered page when admitted, a login redirect or a
    /// refusing redirect when refused.
    UiAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    Forbidden,
    Hidden,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    None,
    RoomDirect,
    RoomAny,
    Group,
}

struct Row {
    method: &'static str,
    path: &'static str,
    guard: Guard,
    style: Style,
    /// What a signed-in caller below the guard gets when they can see the object.
    refusal: Refusal,
    /// What a caller outside the object's scope gets instead.
    outsider: Refusal,
    scope: Scope,
    /// How much of the room a caller reaches decides 403 versus 404 here.
    loose: bool,
    /// The route can answer 404 for an admitted caller too (an unknown catalogue).
    tolerant: bool,
}

const ROWS: &[Row] = &[
    row("GET", "/docs", Guard::Public, Style::Api),
    row("GET", "/", Guard::Public, Style::UiAction),
    row("GET", "/docs/", Guard::Public, Style::Api),
    row("GET", "/docs/{swagger}", Guard::Public, Style::Api),
    row("GET", "/openapi.json", Guard::Public, Style::Api),
    row("POST", "/auth/jwt/login", Guard::Public, Style::Api),
    row("POST", "/auth/jwt/logout", Guard::Public, Style::Api),
    row("GET", "/rooms", Guard::SignedIn, Style::Api),
    row("GET", "/room/{room}", Guard::RoomViewer, Style::Ws).room_any(),
    row("GET", "/room/{room}/users", Guard::RoomViewer, Style::Api).room_any(),
    row("POST", "/auth/register", Guard::Public, Style::Api),
    row(
        "POST",
        "/auth/request-verify-token",
        Guard::Public,
        Style::Api,
    ),
    row("POST", "/auth/forgot-password", Guard::Public, Style::Api),
    row("POST", "/auth/verify", Guard::Public, Style::Api),
    row("POST", "/auth/reset-password", Guard::Public, Style::Api),
    row("GET", "/users/me", Guard::SignedIn, Style::Api),
    row("PATCH", "/users/me", Guard::SignedIn, Style::Api),
    row("GET", "/users/{user}", Guard::Superuser, Style::Api),
    row("PATCH", "/users/{user}", Guard::Superuser, Style::Api),
    row("DELETE", "/users/{user}", Guard::Superuser, Style::Api),
    row("GET", "/api/me", Guard::SignedIn, Style::Api),
    row("GET", "/api/rooms", Guard::SignedIn, Style::Api),
    row("POST", "/api/rooms", Guard::SignedIn, Style::Api),
    row("PATCH", "/api/rooms/{room}", Guard::RoomOwner, Style::Api).room_any(),
    row("DELETE", "/api/rooms/{room}", Guard::RoomOwner, Style::Api).room_any(),
    row(
        "GET",
        "/api/rooms/{room}/members",
        Guard::RoomViewer,
        Style::Api,
    )
    .room_any(),
    row(
        "PATCH",
        "/api/rooms/{room}/members/{member}",
        Guard::RoomOwner,
        Style::Api,
    )
    .room_any(),
    row(
        "DELETE",
        "/api/rooms/{room}/members/{member}",
        Guard::RoomOwner,
        Style::Api,
    )
    .room_any(),
    row(
        "POST",
        "/api/rooms/{room}/owner",
        Guard::RoomOwner,
        Style::Api,
    )
    .room_any(),
    row(
        "GET",
        "/api/rooms/{room}/catalogues",
        Guard::RoomViewer,
        Style::Api,
    )
    .room_any(),
    row(
        "GET",
        "/api/rooms/{room}/catalogues/{catalogue}",
        Guard::RoomViewer,
        Style::Api,
    )
    .room_any()
    .tolerant(),
    row(
        "GET",
        "/api/rooms/{room}/catalogues/{catalogue}/events",
        Guard::RoomViewer,
        Style::Api,
    )
    .room_any()
    .tolerant(),
    row(
        "GET",
        "/api/rooms/{room}/history",
        Guard::RoomViewer,
        Style::Api,
    )
    .room_any(),
    row(
        "GET",
        "/api/rooms/{room}/history/{version}",
        Guard::RoomViewer,
        Style::Api,
    )
    .room_any()
    .tolerant(),
    row(
        "GET",
        "/api/rooms/{room}/history/{version}/state",
        Guard::RoomViewer,
        Style::Api,
    )
    .room_any()
    .tolerant(),
    row(
        "PUT",
        "/api/rooms/{room}/history/{version}/name",
        Guard::RoomEditor,
        Style::Api,
    )
    .room_any()
    .tolerant(),
    row(
        "DELETE",
        "/api/rooms/{room}/history/{version}/name",
        Guard::RoomEditor,
        Style::Api,
    )
    .room_any()
    .tolerant(),
    row("GET", "/api/groups", Guard::SignedIn, Style::Api),
    row("POST", "/api/groups", Guard::SignedIn, Style::Api),
    row(
        "PATCH",
        "/api/groups/{group}",
        Guard::GroupOwner,
        Style::Api,
    )
    .group_scope(),
    row(
        "DELETE",
        "/api/groups/{group}",
        Guard::GroupOwner,
        Style::Api,
    )
    .group_scope(),
    row(
        "GET",
        "/api/groups/{group}/members",
        Guard::GroupMember,
        Style::Api,
    )
    .group_scope(),
    row(
        "DELETE",
        "/api/groups/{group}/members/{member}",
        Guard::GroupOwner,
        Style::Api,
    )
    .group_scope()
    .loose(),
    row(
        "GET",
        "/api/groups/{group}/invitations",
        Guard::GroupOwner,
        Style::Api,
    )
    .group_scope(),
    row(
        "POST",
        "/api/groups/{group}/invitations",
        Guard::GroupOwner,
        Style::Api,
    )
    .group_scope(),
    row(
        "DELETE",
        "/api/groups/{group}/invitations/{group_invitation}",
        Guard::GroupOwner,
        Style::Api,
    )
    .group_scope(),
    row(
        "POST",
        "/api/group-invitations/preview",
        Guard::Public,
        Style::Api,
    ),
    row(
        "POST",
        "/api/group-invitations/accept",
        Guard::SignedIn,
        Style::Api,
    ),
    row(
        "GET",
        "/api/rooms/{room}/groups",
        Guard::RoomViewer,
        Style::Api,
    )
    .room_any(),
    row(
        "PUT",
        "/api/rooms/{room}/groups/{spare}",
        Guard::RoomOwner,
        Style::Api,
    )
    .room_any(),
    row(
        "DELETE",
        "/api/rooms/{room}/groups/{group}",
        Guard::RoomOwner,
        Style::Api,
    )
    .room_direct()
    .loose(),
    row(
        "GET",
        "/api/rooms/{room}/invitations",
        Guard::RoomOwner,
        Style::Api,
    )
    .room_any(),
    row(
        "POST",
        "/api/rooms/{room}/invitations",
        Guard::RoomOwner,
        Style::Api,
    )
    .room_any(),
    row(
        "DELETE",
        "/api/rooms/{room}/invitations/{invitation}",
        Guard::RoomOwner,
        Style::Api,
    )
    .room_any(),
    row(
        "POST",
        "/api/invitations/preview",
        Guard::Public,
        Style::Api,
    ),
    row(
        "POST",
        "/api/invitations/accept",
        Guard::SignedIn,
        Style::Api,
    ),
    row("GET", "/ui/", Guard::Public, Style::UiAction),
    row("GET", "/ui/login", Guard::Public, Style::UiPage),
    row("POST", "/ui/login", Guard::Public, Style::UiAction),
    row("POST", "/ui/logout", Guard::Public, Style::UiAction),
    row("POST", "/ui/request-verify", Guard::Public, Style::UiAction),
    row("GET", "/ui/signup", Guard::Public, Style::UiPage),
    row("POST", "/ui/signup", Guard::Public, Style::UiAction),
    row("GET", "/ui/forgot-password", Guard::Public, Style::UiPage),
    row(
        "POST",
        "/ui/forgot-password",
        Guard::Public,
        Style::UiAction,
    ),
    row("GET", "/ui/reset-password", Guard::Public, Style::UiPage),
    row("POST", "/ui/reset-password", Guard::Public, Style::UiAction),
    row("GET", "/ui/verify", Guard::Public, Style::UiPage),
    row("POST", "/ui/verify", Guard::Public, Style::UiAction),
    row("GET", "/ui/invitation", Guard::Public, Style::UiPage),
    row("POST", "/ui/invitation", Guard::Public, Style::UiAction),
    row(
        "POST",
        "/ui/invitation/accept",
        Guard::SignedIn,
        Style::UiAction,
    ),
    row("GET", "/ui/rooms", Guard::SignedIn, Style::UiPage),
    row("POST", "/ui/rooms", Guard::SignedIn, Style::UiAction),
    row("GET", "/ui/rooms/{room}", Guard::RoomViewer, Style::UiPage).room_any(),
    row(
        "POST",
        "/ui/rooms/{room}/rename",
        Guard::RoomOwner,
        Style::UiAction,
    )
    .room_any(),
    row(
        "POST",
        "/ui/rooms/{room}/delete",
        Guard::RoomOwner,
        Style::UiAction,
    )
    .room_any(),
    row(
        "POST",
        "/ui/rooms/{room}/leave",
        Guard::RoomViewer,
        Style::UiAction,
    )
    .room_any(),
    row(
        "POST",
        "/ui/rooms/{room}/members/{member}/role",
        Guard::RoomOwner,
        Style::UiAction,
    )
    .room_any(),
    row(
        "POST",
        "/ui/rooms/{room}/members/{member}/remove",
        Guard::RoomOwner,
        Style::UiAction,
    )
    .room_any(),
    row(
        "POST",
        "/ui/rooms/{room}/members/{member}/owner",
        Guard::RoomOwner,
        Style::UiAction,
    )
    .room_any(),
    row(
        "POST",
        "/ui/rooms/{room}/invitations",
        Guard::RoomOwner,
        Style::UiAction,
    )
    .room_any(),
    row(
        "POST",
        "/ui/rooms/{room}/groups",
        Guard::RoomOwner,
        Style::UiAction,
    )
    .room_any(),
    row(
        "POST",
        "/ui/rooms/{room}/groups/{group}/remove",
        Guard::RoomOwner,
        Style::UiAction,
    )
    .room_any(),
    row(
        "POST",
        "/ui/rooms/{room}/invitations/{invitation}/revoke",
        Guard::RoomOwner,
        Style::UiAction,
    )
    .room_any(),
    row("GET", "/ui/groups", Guard::SignedIn, Style::UiPage),
    row("POST", "/ui/groups", Guard::SignedIn, Style::UiAction),
    row("GET", "/ui/groups/join", Guard::Public, Style::UiPage),
    row("POST", "/ui/groups/join", Guard::Public, Style::UiAction),
    row(
        "POST",
        "/ui/groups/join/accept",
        Guard::SignedIn,
        Style::UiAction,
    ),
    row(
        "GET",
        "/ui/groups/{group}",
        Guard::GroupMember,
        Style::UiPage,
    )
    .room_any(),
    row(
        "POST",
        "/ui/groups/{group}/rename",
        Guard::GroupOwner,
        Style::UiAction,
    )
    .room_any(),
    row(
        "POST",
        "/ui/groups/{group}/delete",
        Guard::GroupOwner,
        Style::UiAction,
    )
    .room_any(),
    row(
        "POST",
        "/ui/groups/{group}/leave",
        Guard::GroupMember,
        Style::UiAction,
    )
    .room_any(),
    row(
        "POST",
        "/ui/groups/{group}/members/{member}/remove",
        Guard::GroupOwner,
        Style::UiAction,
    )
    .room_any(),
    row(
        "POST",
        "/ui/groups/{group}/invitations",
        Guard::GroupOwner,
        Style::UiAction,
    )
    .room_any(),
    row(
        "POST",
        "/ui/groups/{group}/invitations/{group_invitation}/revoke",
        Guard::GroupOwner,
        Style::UiAction,
    )
    .room_any(),
    row("GET", "/ui/account", Guard::SignedIn, Style::UiPage),
    row(
        "POST",
        "/ui/account/password",
        Guard::SignedIn,
        Style::UiAction,
    ),
    row("GET", "/ui/confirm-email", Guard::SignedIn, Style::UiAction),
    row(
        "POST",
        "/ui/confirm-email",
        Guard::SignedIn,
        Style::UiAction,
    ),
    row(
        "GET",
        "/ui/oauth/{name}/login",
        Guard::Public,
        Style::UiAction,
    ),
    row(
        "GET",
        "/ui/oauth/{name}/callback",
        Guard::Public,
        Style::UiAction,
    ),
    row("GET", "/ui/static/{file}", Guard::Public, Style::UiPage),
    row("GET", "/ui/admin", Guard::Superuser, Style::UiPage),
    row("GET", "/ui/admin/users", Guard::Superuser, Style::UiPage),
    row("GET", "/ui/admin/rooms", Guard::Superuser, Style::UiPage),
    row(
        "POST",
        "/ui/admin/users/{user}/flag",
        Guard::Superuser,
        Style::UiAction,
    ),
    row(
        "POST",
        "/ui/admin/users/{user}/quota",
        Guard::Superuser,
        Style::UiAction,
    ),
    row(
        "POST",
        "/ui/admin/users/{user}/delete",
        Guard::Superuser,
        Style::UiAction,
    ),
    row(
        "POST",
        "/ui/admin/rooms/{room}/limit",
        Guard::Superuser,
        Style::UiAction,
    ),
    row(
        "POST",
        "/ui/admin/rooms/{room}/owner",
        Guard::Superuser,
        Style::UiAction,
    ),
    row(
        "POST",
        "/ui/admin/rooms/{room}/delete",
        Guard::Superuser,
        Style::UiAction,
    ),
];

const fn row(method: &'static str, path: &'static str, guard: Guard, style: Style) -> Row {
    Row {
        method,
        path,
        guard,
        style,
        refusal: Refusal::Forbidden,
        outsider: Refusal::Hidden,
        scope: Scope::None,
        loose: false,
        tolerant: false,
    }
}

impl Row {
    const fn room_direct(mut self) -> Self {
        self.scope = Scope::RoomDirect;
        self
    }

    const fn room_any(mut self) -> Self {
        self.scope = Scope::RoomAny;
        self
    }

    const fn group_scope(mut self) -> Self {
        self.scope = Scope::Group;
        self
    }

    const fn loose(mut self) -> Self {
        self.loose = true;
        self
    }

    const fn tolerant(mut self) -> Self {
        self.tolerant = true;
        self
    }
}

struct Caps {
    signed_in: bool,
    direct: Option<Role>,
    room: Option<Role>,
    group: Option<groups::GroupRole>,
    superuser: bool,
}

impl Caps {
    fn of(kind: Kind) -> Self {
        let room = match kind {
            Kind::Viewer | Kind::GroupViewer => Some(Role::Viewer),
            Kind::Editor | Kind::GroupEditor | Kind::GroupOwner => Some(Role::Editor),
            Kind::Owner => Some(Role::Owner),
            _ => None,
        };
        let group = match kind {
            Kind::GroupEditor => Some(groups::GroupRole::Member),
            Kind::GroupOwner => Some(groups::GroupRole::Owner),
            _ => None,
        };
        let direct = match kind {
            Kind::Viewer => Some(Role::Viewer),
            Kind::Editor => Some(Role::Editor),
            Kind::Owner => Some(Role::Owner),
            _ => None,
        };
        Self {
            signed_in: !matches!(kind, Kind::Anonymous | Kind::Deactivated),
            direct,
            room,
            group,
            superuser: kind == Kind::Superuser,
        }
    }

    fn admits(&self, guard: Guard) -> bool {
        match guard {
            Guard::Public => true,
            Guard::SignedIn => self.signed_in,
            Guard::RoomViewer => self.room.is_some(),
            Guard::RoomEditor => self.room.is_some_and(|role| role.can_write()),
            Guard::RoomOwner => self.room == Some(Role::Owner),
            Guard::GroupMember => self.group.is_some(),
            Guard::GroupOwner => self.group == Some(groups::GroupRole::Owner),
            Guard::Superuser => self.superuser,
        }
    }
}

struct World {
    state: AppState,
    room: String,
    group: String,
    spare: String,
    user: Uuid,
    target: Uuid,
    group_invitation: i64,
    invitation_token: String,
    group_invitation_token: String,
    invitation: i64,
    tokens: Vec<(Kind, String)>,
    _dir: TempDir,
}

impl World {
    async fn start(providers: &[oidc::Provider], hashed: &str) -> Self {
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
            oidc: providers.to_vec(),
        };
        let owner = user(&state, "owner@example.org", hashed).await;
        let editor = user(&state, "editor@example.org", hashed).await;
        let viewer = user(&state, "viewer@example.org", hashed).await;
        let stranger = user(&state, "stranger@example.org", hashed).await;
        let group_owner = user(&state, "group-owner@example.org", hashed).await;
        let group_editor = user(&state, "group-editor@example.org", hashed).await;
        let group_viewer = user(&state, "group-viewer@example.org", hashed).await;
        let unverified = unverified(&state, "unverified@example.org", hashed).await;
        let deactivated = deactivated(&state, "deactivated@example.org", hashed).await;
        let admin = superuser(&state, "admin@example.org", hashed).await;

        let target = user(&state, "target@example.org", hashed).await;
        let room = lifecycle::create_room(&state, owner, "lab").await.unwrap();
        db::add_member(&state.pool, &room.id, target, Role::Viewer)
            .await
            .unwrap();
        db::add_member(&state.pool, &room.id, editor, Role::Editor)
            .await
            .unwrap();
        db::add_member(&state.pool, &room.id, viewer, Role::Viewer)
            .await
            .unwrap();

        let group = groups::create(&state, owner, "friends").await.unwrap();
        member(&state, &group.id, group_editor).await;
        member(&state, &group.id, group_owner).await;
        member(&state, &group.id, target).await;
        groups::grant(&state, &room.id, &group.id, owner, Role::Editor)
            .await
            .unwrap();
        leave_group(&state, &group.id, owner).await;
        promote(&state, &group.id, group_owner).await;
        let spare = groups::create(&state, owner, "spare").await.unwrap();
        let watchers = groups::create(&state, owner, "watchers").await.unwrap();
        member(&state, &watchers.id, group_viewer).await;
        groups::grant(&state, &room.id, &watchers.id, owner, Role::Viewer)
            .await
            .unwrap();

        let (invitation, invitation_token) =
            invitations::create(&state, &room.id, owner, Role::Viewer, None, 14, None)
                .await
                .unwrap();
        let (group_invitation, group_invitation_token) =
            groups::create_invitation(&state, &group.id, group_owner, None, 14, None)
                .await
                .unwrap();

        let tokens = vec![
            (Kind::Anonymous, String::new()),
            (Kind::Unverified, token(&state, unverified).await),
            (Kind::Deactivated, token(&state, deactivated).await),
            (Kind::Stranger, token(&state, stranger).await),
            (Kind::Viewer, token(&state, viewer).await),
            (Kind::Editor, token(&state, editor).await),
            (Kind::Owner, token(&state, owner).await),
            (Kind::GroupViewer, token(&state, group_viewer).await),
            (Kind::GroupEditor, token(&state, group_editor).await),
            (Kind::GroupOwner, token(&state, group_owner).await),
            (Kind::Superuser, token(&state, admin).await),
        ];
        Self {
            state,
            room: room.id,
            group: group.id,
            spare: spare.id,
            user: stranger,
            target,
            group_invitation,
            invitation_token,
            group_invitation_token,
            invitation,
            tokens,
            _dir: dir,
        }
    }

    fn token(&self, kind: Kind) -> Option<String> {
        self.tokens
            .iter()
            .find(|(named, _)| *named == kind)
            .map(|(_, token)| token.clone())
    }

    fn path(&self, template: &str) -> String {
        template
            .replace("{room}", &self.room)
            .replace("{group_invitation}", &self.group_invitation.to_string())
            .replace("{spare}", &self.spare)
            .replace("{group}", &self.group)
            .replace("{catalogue}", "missing")
            .replace("{version}", "0")
            .replace("{invitation}", &self.invitation.to_string())
            .replace("{user}", &self.user.to_string())
            .replace("{member}", &self.target.to_string())
            .replace("{name}", "orcid")
            .replace("{file}", "style.css")
            .replace("{swagger}", "swagger-ui.css")
    }

    async fn call(&self, row: &Row, token: Option<&str>) -> (StatusCode, Option<String>) {
        let path = self.path(row.path);
        if row.style == Style::Ws {
            return self.handshake(token).await;
        }
        let mut builder = Request::builder().method(row.method).uri(&path);
        if let Some(token) = token {
            builder = builder.header(header::COOKIE, format!("fastapiusersauth={token}"));
        }
        let body = match self.body(row) {
            Some((content_type, body)) => {
                builder = builder.header(header::CONTENT_TYPE, content_type);
                Body::from(body)
            }
            None => Body::empty(),
        };
        let mut request = builder.body(body).unwrap();
        request.extensions_mut().insert(axum::extract::ConnectInfo(
            PEER.parse::<std::net::SocketAddr>().unwrap(),
        ));
        let response = ws::router(self.state.clone())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let location = response
            .headers()
            .get(header::LOCATION)
            .map(|value| value.to_str().unwrap_or_default().to_owned());
        (status, location)
    }
}

impl World {
    fn body(&self, row: &Row) -> Option<(&'static str, String)> {
        let form = "application/x-www-form-urlencoded";
        let json = "application/json";
        let body = match (row.method, row.path) {
            ("POST", "/auth/jwt/login") => (
                json,
                format!(r#"{{"username":"owner@example.org","password":"{PASSWORD}"}}"#),
            ),
            ("POST", "/auth/register") => (
                json,
                r#"{"email":"fresh@example.org","password":"another-password"}"#.to_owned(),
            ),
            ("POST", "/auth/request-verify-token" | "/auth/forgot-password") => {
                (json, r#"{"email":"owner@example.org"}"#.to_owned())
            }
            ("POST", "/auth/verify") => (json, r#"{"token":"nope"}"#.to_owned()),
            ("POST", "/api/invitations/preview" | "/api/invitations/accept") => {
                (json, format!(r#"{{"token":"{}"}}"#, self.invitation_token))
            }
            ("POST", "/api/group-invitations/preview" | "/api/group-invitations/accept") => (
                json,
                format!(r#"{{"token":"{}"}}"#, self.group_invitation_token),
            ),
            ("POST", "/auth/reset-password") => (
                json,
                r#"{"token":"nope","password":"another-password"}"#.to_owned(),
            ),
            ("PATCH", "/users/me") => (json, r#"{"password":"another-password"}"#.to_owned()),
            ("PATCH", "/users/{user}") => (json, r#"{"is_active":true}"#.to_owned()),
            ("POST", "/api/rooms" | "/api/groups") => (json, r#"{"name":"another"}"#.to_owned()),
            ("PATCH", "/api/rooms/{room}" | "/api/groups/{group}") => {
                (json, r#"{"name":"renamed"}"#.to_owned())
            }
            ("PATCH", "/api/rooms/{room}/members/{member}") => {
                (json, r#"{"role":"viewer"}"#.to_owned())
            }
            ("PUT", "/api/rooms/{room}/history/{version}/name") => {
                (json, r#"{"name":"paper v1"}"#.to_owned())
            }
            ("POST", "/api/rooms/{room}/owner") => {
                (json, format!(r#"{{"user_id":"{}"}}"#, self.target))
            }
            ("PUT", "/api/rooms/{room}/groups/{spare}") => {
                (json, r#"{"role":"viewer"}"#.to_owned())
            }
            ("POST", "/api/rooms/{room}/invitations") => (json, r#"{"role":"viewer"}"#.to_owned()),
            ("POST", "/api/groups/{group}/invitations") => (json, "{}".to_owned()),
            ("POST", "/ui/login") => (
                form,
                format!("username=owner@example.org&password={PASSWORD}"),
            ),
            ("POST", "/ui/signup") => (
                form,
                "email=brand-new@example.org&password=another-password".to_owned(),
            ),
            ("POST", "/ui/request-verify") => (form, "email=owner@example.org".to_owned()),
            ("POST", "/ui/reset-password") => (
                form,
                "password=another-password&repeat=another-password".to_owned(),
            ),
            (
                "POST",
                "/ui/verify"
                | "/ui/invitation"
                | "/ui/invitation/accept"
                | "/ui/groups/join"
                | "/ui/groups/join/accept",
            ) => (form, "token=nope".to_owned()),
            ("POST", "/ui/rooms") => (form, "name=another".to_owned()),
            ("POST", "/ui/rooms/{room}/rename") => (form, "name=renamed".to_owned()),
            ("POST", "/ui/rooms/{room}/members/{member}/role") => (form, "role=viewer".to_owned()),
            ("POST", "/ui/rooms/{room}/invitations") => (form, "kind=link&role=viewer".to_owned()),
            ("POST", "/ui/rooms/{room}/groups") => {
                (form, format!("group={}&role=viewer", self.spare))
            }
            ("POST", "/ui/groups") => (form, "name=another".to_owned()),
            ("POST", "/ui/groups/{group}/rename") => (form, "name=renamed".to_owned()),
            ("POST", "/ui/groups/{group}/invitations") => (form, "kind=link".to_owned()),
            ("POST", "/ui/account/password") => (
                form,
                format!("current={PASSWORD}&new=another-password&repeat=another-password"),
            ),
            ("POST", "/ui/confirm-email") => (form, "email=fresh@example.org".to_owned()),
            ("POST", "/ui/admin/users/{user}/flag") => {
                (form, "flag=verified&value=true".to_owned())
            }
            ("POST", "/ui/admin/users/{user}/quota" | "/ui/admin/rooms/{room}/limit") => {
                (form, "mb=100".to_owned())
            }
            ("POST", "/ui/admin/rooms/{room}/owner") => {
                (form, "email=owner%40example.org".to_owned())
            }
            ("POST", _) => (form, String::new()),
            _ => return None,
        };
        Some(body)
    }

    /// The websocket route needs a real connection: axum's upgrade extractor refuses a
    /// request that carries no `OnUpgrade` extension, so the handshake is made over TCP.
    async fn handshake(&self, token: Option<&str>) -> (StatusCode, Option<String>) {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = ws::router(self.state.clone());
        let serving = tokio::spawn(async move { axum::serve(listener, app).await });
        let mut request = format!("ws://{address}/room/{}", self.room)
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "sec-websocket-protocol",
            cocat_core::SUBPROTOCOL.parse().unwrap(),
        );
        if let Some(token) = token {
            request.headers_mut().insert(
                header::COOKIE,
                format!("fastapiusersauth={token}").parse().unwrap(),
            );
        }
        let status = match tokio_tungstenite::connect_async(request).await {
            Ok((socket, _)) => {
                drop(socket);
                StatusCode::SWITCHING_PROTOCOLS
            }
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                StatusCode::from_u16(response.status().as_u16()).unwrap()
            }
            Err(_) => StatusCode::BAD_GATEWAY,
        };
        serving.abort();
        (status, None)
    }
}

async fn user(state: &AppState, email: &str, hashed: &str) -> Uuid {
    let id = Uuid::new_v4();
    db::insert_user(&state.pool, id, email, hashed, false, true)
        .await
        .unwrap();
    id
}

async fn unverified(state: &AppState, email: &str, hashed: &str) -> Uuid {
    let id = Uuid::new_v4();
    db::insert_user(&state.pool, id, email, hashed, false, false)
        .await
        .unwrap();
    id
}

async fn deactivated(state: &AppState, email: &str, hashed: &str) -> Uuid {
    let id = user(state, email, hashed).await;
    let mut account = db::user_by_id(&state.pool, id).await.unwrap().unwrap();
    account.is_active = false;
    db::save_user(&state.pool, &account).await.unwrap();
    id
}

async fn superuser(state: &AppState, email: &str, hashed: &str) -> Uuid {
    let id = user(state, email, hashed).await;
    let mut account = db::user_by_id(&state.pool, id).await.unwrap().unwrap();
    account.is_superuser = true;
    db::save_user(&state.pool, &account).await.unwrap();
    id
}

async fn member(state: &AppState, group: &str, user: Uuid) {
    sqlx::query("INSERT INTO group_member (group_id, user_id, role) VALUES (?, ?, 'member')")
        .bind(group)
        .bind(user.to_string())
        .execute(&state.pool)
        .await
        .unwrap();
}

async fn leave_group(state: &AppState, group: &str, user: Uuid) {
    sqlx::query("DELETE FROM group_member WHERE group_id = ? AND user_id = ?")
        .bind(group)
        .bind(user.to_string())
        .execute(&state.pool)
        .await
        .unwrap();
}

async fn promote(state: &AppState, group: &str, user: Uuid) {
    sqlx::query("UPDATE group_member SET role = 'owner' WHERE group_id = ? AND user_id = ?")
        .bind(group)
        .bind(user.to_string())
        .execute(&state.pool)
        .await
        .unwrap();
}

async fn token(state: &AppState, user: Uuid) -> String {
    auth::start_session(&state.pool, user).await.unwrap()
}

#[tokio::test]
async fn every_route_answers_every_caller_as_the_table_says() {
    let fake = oidc_support::Fake::start().await;
    let links = Links::from_env(&|name| {
        (name == "COCAT_PUBLIC_URL").then(|| "http://localhost".to_owned())
    })
    .unwrap();
    let providers = oidc::discover(vec![fake.config(true, "orcid")], &links).await;
    assert_eq!(providers.len(), 1, "the fake provider answered discovery");
    let hashed = db::hash_password(PASSWORD).await.unwrap();
    let mut wrong = Vec::new();
    for row in ROWS {
        for kind in CALLERS {
            let world = World::start(&providers, &hashed).await;
            let caps = Caps::of(kind);
            let token = world.token(kind);
            let (status, location) = world
                .call(
                    row,
                    if caps.signed_in {
                        token.as_deref()
                    } else {
                        None
                    },
                )
                .await;
            if let Err((wanted, _)) = check(row, &caps, status, location.as_deref()) {
                wrong.push(format!(
                    "{} {} as {:?}: expected {}, got {}",
                    row.method, row.path, kind, wanted, status
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} mismatches:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

fn refusal_for(row: &Row, caps: &Caps) -> Refusal {
    let inside = match row.scope {
        Scope::None => true,
        Scope::RoomDirect => caps.direct.is_some(),
        Scope::RoomAny => caps.room.is_some(),
        Scope::Group => caps.group.is_some(),
    };
    if inside { row.refusal } else { row.outsider }
}

/// What the row promises, and why the answer fell outside it.
fn check(
    row: &Row,
    caps: &Caps,
    status: StatusCode,
    location: Option<&str>,
) -> Result<(), (&'static str, String)> {
    if row.guard == Guard::Public {
        return expect(status != StatusCode::NOT_FOUND, "a route that exists");
    }
    if !caps.admits(row.guard) {
        return match row.style {
            Style::Api if !caps.signed_in => expect(status == StatusCode::UNAUTHORIZED, "401"),
            Style::Api if row.loose => expect(
                matches!(status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
                "403 or 404",
            ),
            Style::Api => expect(
                status
                    == match refusal_for(row, caps) {
                        Refusal::Forbidden => StatusCode::FORBIDDEN,
                        Refusal::Hidden => StatusCode::NOT_FOUND,
                    },
                "the row's refusal",
            ),
            Style::Ws => expect(status == StatusCode::FORBIDDEN, "403"),
            Style::UiPage | Style::UiAction if !caps.signed_in => expect(
                matches!(
                    status,
                    StatusCode::TEMPORARY_REDIRECT | StatusCode::SEE_OTHER
                ) && location.is_some_and(|target| target.starts_with("/ui/login")),
                "a redirect to the login page",
            ),
            Style::UiAction => expect(status.is_redirection(), "a refusing redirect"),
            Style::UiPage => expect(status == StatusCode::NOT_FOUND, "404"),
        };
    }
    match row.style {
        Style::Api if row.tolerant => expect(
            !matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN),
            "not 401/403",
        ),
        Style::Api => expect(
            !matches!(
                status,
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
            ),
            "not 401/403/404",
        ),
        Style::Ws => expect(status != StatusCode::FORBIDDEN, "an accepted handshake"),
        Style::UiPage => expect(status == StatusCode::OK, "200"),
        Style::UiAction => expect(
            status.is_success() || status.is_redirection(),
            "a page or a redirect",
        ),
    }
}

fn expect(ok: bool, wanted: &'static str) -> Result<(), (&'static str, String)> {
    if ok {
        Ok(())
    } else {
        Err((wanted, String::new()))
    }
}

fn normalize(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut in_braces = false;
    for character in path.chars() {
        match character {
            '{' => in_braces = true,
            '}' => {
                in_braces = false;
                out.push_str("{}");
            }
            _ if in_braces => {}
            other => out.push(other),
        }
    }
    out
}

#[test]
fn every_route_is_in_the_matrix() {
    let mut missing = Vec::new();
    for (method, path) in registered() {
        let known = ROWS
            .iter()
            .any(|row| row.method == method && normalize(row.path) == path);
        if !known {
            missing.push(format!("{method} {path}"));
        }
    }
    assert!(
        missing.is_empty(),
        "routes without a matrix row:\n{}",
        missing.join("\n")
    );
}

/// Every route the server registers: the JSON API comes from its own document (so a route
/// cannot be served without being published, or published without being served), and the
/// rest from the source of the routers that still spell their paths out.
fn registered() -> Vec<(String, String)> {
    let mut routes = documented();
    routes.extend(hand_written());
    routes
}

/// The JSON API as the generated document lists it.
fn documented() -> Vec<(String, String)> {
    let links = Links::from_env(&|_| None).expect("no environment is a valid link set");
    let document = serde_json::to_value(cocat_server::openapi::document(&links))
        .expect("the document serializes");
    let mut routes = Vec::new();
    for (path, item) in document["paths"]
        .as_object()
        .expect("the document has paths")
    {
        for method in ["get", "put", "post", "delete", "patch", "head", "options"] {
            if item.get(method).is_some() {
                routes.push((method.to_uppercase(), normalize(path)));
            }
        }
    }
    routes
}

/// The routes still declared with `.route(..)`: the rendered pages, the websocket and the
/// documentation itself. axum keeps its route table private, so the source is the only place
/// that knows them.
fn hand_written() -> Vec<(String, String)> {
    const FILES: [(&str, &str); 3] = [("ws.rs", ""), ("openapi.rs", ""), ("ui/mod.rs", "/ui")];
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut routes = Vec::new();
    for (file, prefix) in FILES {
        let source = fs::read_to_string(root.join(file)).unwrap();
        for call in route_calls(&source) {
            let Some((path, rest)) = first_literal(&call) else {
                continue;
            };
            let full = format!("{prefix}{path}");
            for method in methods(&rest) {
                routes.push((method.to_owned(), normalize(&full)));
            }
        }
    }
    routes
}

/// The text inside each `.route(...)`, with nested parentheses respected.
fn route_calls(source: &str) -> Vec<String> {
    let mut calls = Vec::new();
    let mut rest = source;
    while let Some(start) = rest.find(".route(") {
        let after = &rest[start + ".route(".len()..];
        let mut depth = 1;
        let mut end = after.len();
        for (index, character) in after.char_indices() {
            match character {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = index;
                        break;
                    }
                }
                _ => {}
            }
        }
        calls.push(after[..end].to_owned());
        rest = &after[end..];
    }
    calls
}

/// The first string literal of a call, and the text after it (the handler chain).
fn first_literal(call: &str) -> Option<(String, String)> {
    let start = call.find('"')?;
    let end = call[start + 1..].find('"')? + start + 1;
    Some((call[start + 1..end].to_owned(), call[end..].to_owned()))
}

fn methods(call: &str) -> Vec<&'static str> {
    [
        ("get", "GET"),
        ("post", "POST"),
        ("patch", "PATCH"),
        ("put", "PUT"),
        ("delete", "DELETE"),
    ]
    .into_iter()
    .filter(|(lower, _)| call.contains(&format!("{lower}(")))
    .map(|(_, upper)| upper)
    .collect()
}
