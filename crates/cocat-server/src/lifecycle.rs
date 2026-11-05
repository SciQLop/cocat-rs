//! Room actions that change the database and then revoke the sockets the change affects.
//! The rules are Python's `rooms.py`; the revocation lives here so no caller can forget it.

use cocat_core::role::Role;
use thiserror::Error;
use uuid::Uuid;

use crate::AppState;
use crate::db::{self, DbError};

const OWNER_KEEPS_OWNER_ROLE: &str = "The owner keeps the owner role";
const OWNER_CANNOT_LEAVE: &str = "The owner cannot leave or be removed";

#[derive(Debug, Error)]
pub enum LifecycleError {
    #[error("the room or the member does not exist")]
    NotFound,
    #[error("the actor's role is too low for this action")]
    Forbidden,
    #[error("{0}")]
    Conflict(&'static str),
    #[error(transparent)]
    Db(#[from] DbError),
}

/// Deletes the room, its memberships and its file, closing every socket it had.
pub async fn delete_room(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
) -> Result<(), LifecycleError> {
    require_role(state, room_id, actor, Role::Owner).await?;
    db::delete_room(&state.pool, room_id).await?;
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
        return Err(LifecycleError::NotFound);
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
        return Err(LifecycleError::NotFound);
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

async fn require_role(
    state: &AppState,
    room_id: &str,
    actor: Uuid,
    minimum: Role,
) -> Result<(), LifecycleError> {
    let role = db::role_in(&state.pool, room_id, actor)
        .await?
        .ok_or(LifecycleError::NotFound)?;
    if role.rank() < minimum.rank() {
        return Err(LifecycleError::Forbidden);
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
        .ok_or(LifecycleError::NotFound)
}
