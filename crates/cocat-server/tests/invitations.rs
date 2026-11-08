use std::fs;

use cocat_core::role::Role;
use cocat_server::lifecycle::LifecycleError;
use cocat_server::room::RoomManager;
use cocat_server::{AppState, db, invitations, links::Links, ratelimit::LoginLimit};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
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
            web_dir: None,
        };
        Self { state, _dir: dir }
    }

    async fn user(&self, email: &str) -> Uuid {
        db::create_user(&self.state.pool, email, "hunter2", false, true)
            .await
            .unwrap()
    }

    async fn user_row(&self, id: Uuid) -> db::User {
        db::user_by_id(&self.state.pool, id)
            .await
            .unwrap()
            .expect("the user vanished")
    }

    async fn room(&self, room_id: &str, owner: Uuid) {
        db::create_room(&self.state.pool, owner, room_id, "Lab")
            .await
            .unwrap();
    }

    async fn member(&self, room_id: &str, email: &str, role: Role) -> Uuid {
        let user = self.user(email).await;
        db::add_member(&self.state.pool, room_id, user, role)
            .await
            .unwrap();
        user
    }

    async fn invite(
        &self,
        room_id: &str,
        actor: Uuid,
        role: Role,
        email: Option<&str>,
    ) -> (i64, String) {
        invitations::create(&self.state, room_id, actor, role, email, 14, None)
            .await
            .unwrap()
    }
}

/// A token stored by `invite`, for the tests that only need a room and a token.
async fn room_with_invite() -> (Fixture, Uuid, String) {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room("lab", owner).await;
    let (_, token) = fixture.invite("lab", owner, Role::Editor, None).await;
    (fixture, owner, token)
}

#[tokio::test]
async fn an_invitation_admits_the_user_it_previews() {
    let (fixture, owner, token) = room_with_invite().await;
    let guest = fixture.user("guest@example.org").await;

    let preview = invitations::preview(&fixture.state, &token).await.unwrap();
    assert_eq!(preview.room_id, "lab");
    assert_eq!(preview.room_name, "Lab");
    assert_eq!(preview.role, Role::Editor);
    assert_eq!(preview.inviter_email.as_deref(), Some("owner@example.org"));
    assert!(preview.expires_at.ends_with('Z'));

    let (room, role) = invitations::accept(&fixture.state, &token, &fixture.user_row(guest).await)
        .await
        .unwrap();
    assert_eq!(room.id, "lab");
    assert_eq!(room.name, "Lab");
    assert_eq!(role, Role::Editor);
    assert_eq!(
        db::role_in(&fixture.state.pool, "lab", guest)
            .await
            .unwrap(),
        Some(Role::Editor)
    );

    let listed = invitations::list(&fixture.state, "lab", owner)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].uses, 1);
    assert_eq!(listed[0].role, Role::Editor);
}

#[tokio::test]
async fn the_database_stores_only_the_token_hash() {
    let (fixture, _, token) = room_with_invite().await;

    let stored: String = sqlx::query_scalar("SELECT token_hash FROM invitation")
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();

    assert_ne!(stored, token);
    assert_eq!(stored, format!("{:x}", Sha256::digest(token.as_bytes())));
}

#[tokio::test]
async fn an_expired_invitation_is_not_found() {
    let (fixture, _, token) = room_with_invite().await;
    sqlx::query("UPDATE invitation SET expires_at = '2000-01-01T00:00:00.000Z'")
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    let guest = fixture.user("guest@example.org").await;

    let preview = invitations::preview(&fixture.state, &token).await.err();
    let accept = invitations::accept(&fixture.state, &token, &fixture.user_row(guest).await)
        .await
        .err();

    assert!(matches!(preview, Some(LifecycleError::NotFound(_))));
    assert!(matches!(accept, Some(LifecycleError::NotFound(_))));
}

#[tokio::test]
async fn a_revoked_invitation_is_not_found() {
    let (fixture, owner, token) = room_with_invite().await;
    let id: i64 = sqlx::query_scalar("SELECT id FROM invitation")
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();

    invitations::revoke(&fixture.state, "lab", owner, id)
        .await
        .unwrap();

    assert!(
        invitations::list(&fixture.state, "lab", owner)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        invitations::preview(&fixture.state, &token).await,
        Err(LifecycleError::NotFound(_))
    ));
}

#[tokio::test]
async fn an_email_invitation_is_bound_to_that_address() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room("lab", owner).await;
    let (_, token) = fixture
        .invite("lab", owner, Role::Editor, Some("Guest@Example.org"))
        .await;
    let stranger = fixture.user("stranger@example.org").await;
    let guest = fixture.user("guest@example.org").await;

    let wrong =
        invitations::accept(&fixture.state, &token, &fixture.user_row(stranger).await).await;
    let right = invitations::accept(&fixture.state, &token, &fixture.user_row(guest).await).await;

    assert!(matches!(wrong, Err(LifecycleError::Forbidden(_))));
    assert_eq!(right.unwrap().1, Role::Editor);
}

#[tokio::test]
async fn a_single_use_invitation_admits_one_of_two_racing_accepts() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room("lab", owner).await;
    let first = fixture.user("first@example.org").await;
    let second = fixture.user("second@example.org").await;
    let (_, token) = fixture
        .invite("lab", owner, Role::Editor, Some("first@example.org"))
        .await;

    let first_user = fixture.user_row(first).await;
    let second_user = fixture.user_row(second).await;
    let (one, two) = tokio::join!(
        invitations::accept(&fixture.state, &token, &first_user),
        invitations::accept(&fixture.state, &token, &second_user),
    );

    assert_eq!(
        [one.is_ok(), two.is_ok()].iter().filter(|ok| **ok).count(),
        1,
        "exactly one accept must join: {one:?} / {two:?}"
    );
}

/// A membership that cannot be written (here: a user row that does not exist, so the foreign
/// key refuses) must not leave the use spent behind it.
#[tokio::test]
async fn a_failed_membership_does_not_burn_the_use() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room("lab", owner).await;
    let (id, token) = fixture.invite("lab", owner, Role::Editor, None).await;
    let ghost = db::User {
        id: Uuid::new_v4(),
        email: "ghost@example.org".to_owned(),
        hashed_password: "hunter2".to_owned(),
        is_active: true,
        is_superuser: false,
        is_verified: true,
    };

    let error = invitations::accept(&fixture.state, &token, &ghost)
        .await
        .err();
    let uses: i64 = sqlx::query_scalar("SELECT uses FROM invitation WHERE id = ?")
        .bind(id)
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();

    assert!(matches!(error, Some(LifecycleError::Db(_))), "{error:?}");
    assert_eq!(uses, 0, "the failed accept must give its use back");

    let guest = fixture.user("guest@example.org").await;
    assert_eq!(
        invitations::accept(&fixture.state, &token, &fixture.user_row(guest).await)
            .await
            .unwrap()
            .1,
        Role::Editor
    );
}

#[tokio::test]
async fn revoking_twice_is_allowed_but_an_unknown_id_is_not() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room("lab", owner).await;
    let (id, _) = fixture.invite("lab", owner, Role::Editor, None).await;

    invitations::revoke(&fixture.state, "lab", owner, id)
        .await
        .unwrap();
    invitations::revoke(&fixture.state, "lab", owner, id)
        .await
        .unwrap();

    let unknown = invitations::revoke(&fixture.state, "lab", owner, id + 1)
        .await
        .err();
    assert!(
        matches!(&unknown, Some(LifecycleError::NotFound(message))
            if message == &format!("Invitation {} not found", id + 1)),
        "{unknown:?}"
    );
}

#[tokio::test]
async fn accepting_a_lower_invitation_keeps_the_higher_role() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room("lab", owner).await;
    let (_, token) = fixture.invite("lab", owner, Role::Viewer, None).await;

    let (_, role) = invitations::accept(&fixture.state, &token, &fixture.user_row(owner).await)
        .await
        .unwrap();

    assert_eq!(role, Role::Owner);
    assert_eq!(
        db::role_in(&fixture.state.pool, "lab", owner)
            .await
            .unwrap(),
        Some(Role::Owner)
    );
}

#[tokio::test]
async fn an_invitation_can_grant_the_viewer_role() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room("lab", owner).await;
    let (_, token) = fixture.invite("lab", owner, Role::Viewer, None).await;
    let guest = fixture.user("guest@example.org").await;

    let (_, role) = invitations::accept(&fixture.state, &token, &fixture.user_row(guest).await)
        .await
        .unwrap();

    assert_eq!(role, Role::Viewer);
}

#[tokio::test]
async fn an_invitation_cannot_grant_owner() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room("lab", owner).await;

    let error = invitations::create(&fixture.state, "lab", owner, Role::Owner, None, 14, None)
        .await
        .err();

    assert!(matches!(
        error,
        Some(LifecycleError::Conflict(
            "An invitation cannot grant the owner role"
        ))
    ));
}

#[tokio::test]
async fn only_the_owner_may_invite_or_revoke() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room("lab", owner).await;
    let editor = fixture
        .member("lab", "editor@example.org", Role::Editor)
        .await;
    let (id, _) = fixture.invite("lab", owner, Role::Editor, None).await;

    let created = invitations::create(&fixture.state, "lab", editor, Role::Editor, None, 14, None)
        .await
        .err();
    let revoked = invitations::revoke(&fixture.state, "lab", editor, id)
        .await
        .err();

    assert!(matches!(created, Some(LifecycleError::Forbidden(_))));
    assert!(matches!(revoked, Some(LifecycleError::Forbidden(_))));
}

#[tokio::test]
async fn a_revoked_invitation_belonging_to_another_room_is_not_found() {
    let fixture = Fixture::start().await;
    let owner = fixture.user("owner@example.org").await;
    fixture.room("lab", owner).await;
    db::create_room(&fixture.state.pool, owner, "other", "Other")
        .await
        .unwrap();
    let (id, _) = fixture.invite("lab", owner, Role::Editor, None).await;

    let error = invitations::revoke(&fixture.state, "other", owner, id)
        .await
        .err();

    assert!(matches!(error, Some(LifecycleError::NotFound(_))));
}
