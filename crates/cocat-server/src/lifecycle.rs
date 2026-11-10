//! Room actions that change the database and then revoke the sockets the change affects.
//! The rules are Python's `rooms.py`; the revocation lives here so no caller can forget it.

use cocat_core::role::Role;
use thiserror::Error;
use uuid::Uuid;

use crate::AppState;
use crate::db::{self, DbError, RoomRow};
use crate::room::CloseReason;

const OWNER_KEEPS_OWNER_ROLE: &str = "The owner keeps the owner role";
const OWNER_CANNOT_LEAVE: &str = "The owner cannot leave or be removed";
const LONG_NAME: &str = "Enter a name of at most 200 characters";
pub(crate) const MAX_NAME: usize = 200;
pub(crate) const NO_MEMBERS: &str =
    "A public catalogue has no members: it cannot be shared, left or handed over";
pub(crate) const NO_DOCUMENTS: &str =
    "A catalogue project holds one catalogues document: only a workspace holds documents";
pub(crate) const PERSONAL: &str =
    "“My catalogues” is personal: copy catalogues into another project to share them.";

/// The noun a name belongs to, so rooms and groups word the same refusals differently.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Noun {
    Room,
    Group,
}

impl Noun {
    fn empty(self) -> &'static str {
        match self {
            Self::Room => "Enter a project name",
            Self::Group => "Enter a group name",
        }
    }

    fn control_characters(self) -> &'static str {
        match self {
            Self::Room => "Enter a project name without control characters",
            Self::Group => "Enter a group name without control characters",
        }
    }
}

#[derive(Debug, Error)]
pub enum LifecycleError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    Conflict(&'static str),
    #[error("{0}")]
    Invalid(&'static str),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Role(#[from] cocat_core::role::UnknownRole),
    #[error("the random source failed: {0}")]
    Random(#[from] getrandom::Error),
}

impl From<sqlx::Error> for LifecycleError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(DbError::from(error))
    }
}

/// Creates a catalogue project owned by `owner`.
pub async fn create_room(
    state: &AppState,
    owner: Uuid,
    name: &str,
) -> Result<RoomRow, LifecycleError> {
    create_project(state, owner, name, db::ProjectKind::Catalogues).await
}

/// Creates a project of `kind` owned by `owner`; a workspace gets its manifest with it. The
/// id is the hex UUID Python's `uuid4().hex` gives.
pub async fn create_project(
    state: &AppState,
    owner: Uuid,
    name: &str,
    kind: db::ProjectKind,
) -> Result<RoomRow, LifecycleError> {
    let id = Uuid::new_v4().simple().to_string();
    let name = clean_name(name, Noun::Room)?;
    match kind {
        db::ProjectKind::Catalogues => {
            db::create_room(&state.pool, owner, &id, &name, kind).await?
        }
        db::ProjectKind::Workspace => {
            db::create_workspace(&state.pool, owner, &id, &name).await?;
        }
    }
    Ok(RoomRow { id, name })
}

/// Adds an empty notebook or text document to a workspace; its owner and editors may. The
/// manifest is made with the workspace, so a second one is refused.
pub async fn add_document(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
    kind: db::DocumentKind,
) -> Result<Uuid, LifecycleError> {
    require_role(state, room_id, actor, Role::Editor).await?;
    require_workspace(state, room_id).await?;
    if kind == db::DocumentKind::Manifest {
        return Err(LifecycleError::Invalid(
            "A workspace has one manifest, made with it",
        ));
    }
    Ok(db::create_document(&state.pool, room_id, kind, actor).await?)
}

/// Refuses a project that is not a workspace: only workspaces hold documents.
pub(crate) async fn require_workspace(
    state: &AppState,
    room_id: &str,
) -> Result<(), LifecycleError> {
    match db::project_kind(&state.pool, room_id).await? {
        Some(db::ProjectKind::Workspace) => Ok(()),
        Some(db::ProjectKind::Catalogues) => Err(LifecycleError::Conflict(NO_DOCUMENTS)),
        None => Err(room_not_found(room_id)),
    }
}

/// Renames a room; its owner and its editors may. Answers the room and the actor's role.
pub async fn rename_room(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
    name: &str,
) -> Result<(RoomRow, Role), LifecycleError> {
    let role = require_role(state, room_id, actor, Role::Editor).await?;
    let name = clean_name(name, Noun::Room)?;
    if !db::rename_room(&state.pool, room_id, &name).await? {
        return Err(room_not_found(room_id));
    }
    let room = RoomRow {
        id: room_id.to_owned(),
        name,
    };
    Ok((room, role))
}

/// Lets the room's editors share it, or keeps sharing to the owner; only the owner decides.
pub async fn set_editors_can_share(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
    allowed: bool,
) -> Result<(), LifecycleError> {
    refuse_public(state, room_id).await?;
    require_role(state, room_id, actor, Role::Owner).await?;
    if !db::set_editors_can_share(&state.pool, room_id, allowed).await? {
        return Err(room_not_found(room_id));
    }
    Ok(())
}

/// Python's `rooms.py::_room_name`, shared with groups: trim, then refuse an empty name, a
/// control character or anything over 200 characters.
pub(crate) fn clean_name(name: &str, noun: Noun) -> Result<String, LifecycleError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(LifecycleError::Invalid(noun.empty()));
    }
    if trimmed
        .chars()
        .any(|character| character < ' ' || character == '\x7f')
    {
        return Err(LifecycleError::Invalid(noun.control_characters()));
    }
    if trimmed.chars().count() > MAX_NAME {
        return Err(LifecycleError::Invalid(LONG_NAME));
    }
    Ok(trimmed.to_owned())
}

/// Deletes the room, its memberships and its file, closing every socket it had. Its owner
/// may, and a curator for a public catalogue, which has no owner.
pub async fn delete_room(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
) -> Result<(), LifecycleError> {
    let deleter = match db::is_public(&state.pool, room_id).await? {
        true => Role::Editor,
        false => Role::Owner,
    };
    require_role(state, room_id, actor, deleter).await?;
    if !remove_project(state, room_id).await? {
        return Err(room_not_found(room_id));
    }
    Ok(())
}

/// Deletes the project row, then the files of its catalogues and documents. The documents are
/// read first because the row takes them with it. False when there was no such project.
pub(crate) async fn remove_project(state: &AppState, room_id: &str) -> Result<bool, DbError> {
    let documents: Vec<Uuid> = db::documents_of(&state.pool, room_id)
        .await?
        .into_iter()
        .map(|(doc, _)| doc)
        .collect();
    if !db::delete_room(&state.pool, room_id).await? {
        return Ok(false);
    }
    state.rooms.delete_project(room_id, &documents).await;
    Ok(true)
}

/// Changes a member's role and closes their sockets, which were admitted under the old one.
pub async fn set_member_role(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
    member: Uuid,
    role: Role,
) -> Result<(), LifecycleError> {
    require_sharer(state, room_id, actor).await?;
    let current = member_role(state, room_id, member).await?;
    if role == Role::Owner || current == Role::Owner {
        return Err(LifecycleError::Conflict(OWNER_KEEPS_OWNER_ROLE));
    }
    if !db::set_role(&state.pool, room_id, member, role).await? {
        return Err(not_a_member(room_id, member));
    }
    state
        .rooms
        .revoke_project(&state.pool, room_id, member, CloseReason::AccessChanged)
        .await;
    Ok(())
}

/// Removes a member: whoever may share removes anyone but the owner, and every member may
/// leave.
pub async fn remove_member(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
    member: Uuid,
) -> Result<(), LifecycleError> {
    require_removal(state, room_id, actor, member).await?;
    if member_role(state, room_id, member).await? == Role::Owner {
        return Err(LifecycleError::Conflict(OWNER_CANNOT_LEAVE));
    }
    if !db::remove_member(&state.pool, room_id, member).await? {
        return Err(not_a_member(room_id, member));
    }
    state
        .rooms
        .revoke_project(&state.pool, room_id, member, CloseReason::AccessChanged)
        .await;
    Ok(())
}

/// Hands the room to a member. Their sockets drop: a socket keeps the role it attached with.
pub async fn transfer_ownership(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
    new_owner: Uuid,
) -> Result<(), LifecycleError> {
    refuse_public(state, room_id).await?;
    require_role(state, room_id, actor, Role::Owner).await?;
    refuse_personal(state, room_id).await?;
    member_role(state, room_id, new_owner).await?;
    db::set_owner(&state.pool, room_id, new_owner).await?;
    state
        .rooms
        .revoke_project(&state.pool, room_id, new_owner, CloseReason::AccessChanged)
        .await;
    Ok(())
}

/// Leaving needs only membership; removing someone else needs the right to share.
pub(crate) async fn require_removal(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
    member: Uuid,
) -> Result<(), LifecycleError> {
    refuse_public(state, room_id).await?;
    match member == actor {
        true => require_role(state, room_id, actor, Role::Viewer)
            .await
            .map(drop),
        false => require_sharer(state, room_id, actor).await.map(drop),
    }
}

/// The one rule for changing who has access to a room: see [`Role::may_share`].
pub(crate) async fn require_sharer(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
) -> Result<Role, LifecycleError> {
    refuse_public(state, room_id).await?;
    let role = require_role(state, room_id, actor, Role::Viewer).await?;
    let open = db::editors_can_share(&state.pool, room_id)
        .await?
        .unwrap_or(false);
    if !role.may_share(open) {
        return Err(LifecycleError::Forbidden(format!(
            "Only {} can share project {room_id}",
            Role::sharers(open)
        )));
    }
    Ok(role)
}

/// Who has access to a public catalogue is not a list anyone keeps: every account reads it
/// and curators edit it. So members, invitations, groups and sharing refuse it for everyone.
pub(crate) async fn refuse_public(state: &AppState, room_id: &str) -> Result<(), LifecycleError> {
    match db::is_public(&state.pool, room_id).await? {
        true => Err(LifecycleError::Conflict(NO_MEMBERS)),
        false => Ok(()),
    }
}

/// An account's own project gains nobody: no invitation, member, group or new owner. Who
/// was in it before this rule stays, and can still be removed.
pub(crate) async fn refuse_personal(state: &AppState, room_id: &str) -> Result<(), LifecycleError> {
    match db::is_personal_room(&state.pool, room_id).await? {
        true => Err(LifecycleError::Conflict(PERSONAL)),
        false => Ok(()),
    }
}

/// The actor's role in the room, refused when it is below `minimum`.
pub(crate) async fn require_role(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
    minimum: Role,
) -> Result<Role, LifecycleError> {
    let role = db::role_in(&state.pool, room_id, actor)
        .await?
        .ok_or_else(|| room_not_found(room_id))?;
    if role.rank() < minimum.rank() {
        return Err(LifecycleError::Forbidden(format!(
            "Project {room_id} requires the {} role",
            minimum.as_str()
        )));
    }
    Ok(role)
}

async fn member_role(
    state: &AppState,
    room_id: &str,
    member: Uuid,
) -> Result<Role, LifecycleError> {
    db::direct_role(&state.pool, room_id, member)
        .await?
        .ok_or_else(|| not_a_member(room_id, member))
}

fn room_not_found(room_id: &str) -> LifecycleError {
    LifecycleError::NotFound(format!("Project {room_id} not found"))
}

fn not_a_member(room_id: &str, member: Uuid) -> LifecycleError {
    LifecycleError::NotFound(format!(
        "User {member} is not a member of project {room_id}"
    ))
}
