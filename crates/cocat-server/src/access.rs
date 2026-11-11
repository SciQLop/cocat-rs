//! One answer to "which role does this user hold on this room key now". The socket, its
//! re-checks and every route that names a document ask it, so hiding or freezing a proposal
//! is a role change like any other.

use cocat_core::role::Role;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::db::{self, DbError, DocumentKind};
use crate::rounds::rules::{self, Seat, Whose};
use crate::rounds::{self, ProposalDoc};
use crate::{AppState, documents};

/// What a room key of a project holds.
pub enum Content {
    Catalogues,
    Document(DocumentKind),
    Proposal(Box<ProposalDoc>),
}

impl Content {
    /// The kind of a `document` row; catalogues and proposals have none.
    pub fn kind(&self) -> Option<DocumentKind> {
        match self {
            Self::Document(kind) => Some(*kind),
            Self::Catalogues | Self::Proposal(_) => None,
        }
    }
}

/// The project's catalogues when `doc` is `None`, else its document or proposal `doc`.
pub async fn content(
    pool: &SqlitePool,
    project: &str,
    doc: Option<Uuid>,
) -> Result<Option<Content>, DbError> {
    let Some(doc) = doc else {
        return Ok(Some(Content::Catalogues));
    };
    if let Some(kind) = db::document_kind(pool, project, doc).await? {
        return Ok(Some(Content::Document(kind)));
    }
    Ok(rounds::proposal_by_doc(pool, project, doc)
        .await?
        .map(|proposal| Content::Proposal(Box::new(proposal))))
}

/// The role a project member holds on the content: their project role, except on a
/// proposal, where the round's rules decide.
pub async fn role_on(
    pool: &SqlitePool,
    content: &Content,
    project_role: Role,
    user: Uuid,
) -> Result<Option<Role>, DbError> {
    let Content::Proposal(proposal) = content else {
        return Ok(Some(project_role));
    };
    let members = rounds::members(pool, proposal.round.id).await?;
    let seat = Seat {
        project_role,
        roles: rounds::roles_of(&members, user),
    };
    let whose = Whose::of(proposal.author, proposal.candidates, user);
    Ok(rules::proposal_role(&proposal.round, &seat, whose))
}

/// The role `user` holds on the room key now; `None` for no access or an unknown key.
pub async fn key_role(state: &AppState, key: &str, user: Uuid) -> Result<Option<Role>, DbError> {
    let project = documents::project_of(key);
    let doc = key
        .rsplit_once('/')
        .and_then(|(_, doc)| doc.parse::<Uuid>().ok());
    let Some(project_role) = db::role_in(&state.pool, project, user).await? else {
        return Ok(None);
    };
    match content(&state.pool, project, doc).await? {
        Some(content) => role_on(&state.pool, &content, project_role, user).await,
        None => Ok(None),
    }
}
