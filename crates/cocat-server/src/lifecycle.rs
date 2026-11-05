//! Room actions that change the database and then revoke the sockets the change affects.
//! The rules are Python's `rooms.py`; the revocation lives here so no caller can forget it.

use cocat_core::role::Role;
use thiserror::Error;
use uuid::Uuid;

use crate::AppState;
use crate::db::{self, DbError, RoomRow};

const OWNER_KEEPS_OWNER_ROLE: &str = "The owner keeps the owner role";
const OWNER_CANNOT_LEAVE: &str = "The owner cannot leave or be removed";
const EMPTY_ROOM_NAME: &str = "Enter a room name";
const CONTROL_CHARACTERS_IN_ROOM_NAME: &str = "Enter a room name without control characters";

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

/// Creates a room owned by `owner`. The id is the hex UUID Python's `uuid4().hex` gives.
pub async fn create_room(
    state: &AppState,
    owner: Uuid,
    name: &str,
) -> Result<RoomRow, LifecycleError> {
    let id = Uuid::new_v4().simple().to_string();
    let name = room_name(name)?;
    db::create_room(&state.pool, owner, &id, &name).await?;
    Ok(RoomRow { id, name })
}

/// Renames a room; only its owner may.
pub async fn rename_room(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
    name: &str,
) -> Result<RoomRow, LifecycleError> {
    require_role(state, room_id, actor, Role::Owner).await?;
    let name = room_name(name)?;
    if !db::rename_room(&state.pool, room_id, &name).await? {
        return Err(room_not_found(room_id));
    }
    Ok(RoomRow {
        id: room_id.to_owned(),
        name,
    })
}

/// Python's `rooms.py::_room_name`: trim, then refuse an empty name or a control character.
fn room_name(name: &str) -> Result<String, LifecycleError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(LifecycleError::Invalid(EMPTY_ROOM_NAME));
    }
    if trimmed
        .chars()
        .any(|character| character < ' ' || character == '\x7f')
    {
        return Err(LifecycleError::Invalid(CONTROL_CHARACTERS_IN_ROOM_NAME));
    }
    Ok(trimmed.to_owned())
}

/// Deletes the room, its memberships and its file, closing every socket it had.
pub async fn delete_room(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
) -> Result<(), LifecycleError> {
    require_role(state, room_id, actor, Role::Owner).await?;
    if !db::delete_room(&state.pool, room_id).await? {
        return Err(room_not_found(room_id));
    }
    state.rooms.delete(room_id).await;
    Ok(())
}

/// Changes a member's role and closes their sockets, which were admitted under the old one.
pub async fn set_member_role(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
    member: Uuid,
    role: Role,
) -> Result<(), LifecycleError> {
    require_role(state, room_id, actor, Role::Owner).await?;
    let current = member_role(state, room_id, member).await?;
    if role == Role::Owner || current == Role::Owner {
        return Err(LifecycleError::Conflict(OWNER_KEEPS_OWNER_ROLE));
    }
    if !db::set_role(&state.pool, room_id, member, role).await? {
        return Err(not_a_member(room_id, member));
    }
    state.rooms.revoke(room_id, member).await;
    Ok(())
}

/// Removes a member: the owner may remove anyone but themselves, and every member may leave.
pub async fn remove_member(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
    member: Uuid,
) -> Result<(), LifecycleError> {
    let minimum = if member == actor {
        Role::Viewer
    } else {
        Role::Owner
    };
    require_role(state, room_id, actor, minimum).await?;
    if member_role(state, room_id, member).await? == Role::Owner {
        return Err(LifecycleError::Conflict(OWNER_CANNOT_LEAVE));
    }
    if !db::remove_member(&state.pool, room_id, member).await? {
        return Err(not_a_member(room_id, member));
    }
    state.rooms.revoke(room_id, member).await;
    Ok(())
}

/// Hands the room to a member. Their sockets drop: a socket keeps the role it attached with.
pub async fn transfer_ownership(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
    new_owner: Uuid,
) -> Result<(), LifecycleError> {
    require_role(state, room_id, actor, Role::Owner).await?;
    member_role(state, room_id, new_owner).await?;
    db::set_owner(&state.pool, room_id, new_owner).await?;
    state.rooms.revoke(room_id, new_owner).await;
    Ok(())
}

pub(crate) async fn require_role(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
    minimum: Role,
) -> Result<(), LifecycleError> {
    let role = db::role_in(&state.pool, room_id, actor)
        .await?
        .ok_or_else(|| room_not_found(room_id))?;
    if role.rank() < minimum.rank() {
        return Err(LifecycleError::Forbidden(format!(
            "Room {room_id} requires the {} role",
            minimum.as_str()
        )));
    }
    Ok(())
}

async fn member_role(
    state: &AppState,
    room_id: &str,
    member: Uuid,
) -> Result<Role, LifecycleError> {
    db::role_in(&state.pool, room_id, member)
        .await?
        .ok_or_else(|| not_a_member(room_id, member))
}

fn room_not_found(room_id: &str) -> LifecycleError {
    LifecycleError::NotFound(format!("Room {room_id} not found"))
}

fn not_a_member(room_id: &str, member: Uuid) -> LifecycleError {
    LifecycleError::NotFound(format!("User {member} is not a member of room {room_id}"))
}
