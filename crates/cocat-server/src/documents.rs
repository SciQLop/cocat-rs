//! A project's documents: its file list (the manifest), made with it or added once, and the
//! notebooks and texts added after. Their content lives in sockets and storage, never in these routes.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use cocat_core::role::Role;
use serde::{Deserialize, Serialize};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::api::{ApiError, ApiPath, CurrentUser, JsonBody};
use crate::db::{self, DocumentKind, DocumentRow};
use crate::lifecycle;
use crate::openapi::Detail;
use crate::rounds::{self, ProjectProposal};
use crate::{AppState, access};

/// The socket and storage key of one document: `/room/{project}` stays the catalogues
/// document, a workspace document is `{project}/{document}`.
pub fn key(room_id: &str, doc: Uuid) -> String {
    format!("{room_id}/{doc}")
}

/// The project a room key belongs to: the key itself unless it ends in a document id.
pub fn project_of(key: &str) -> &str {
    match key.rsplit_once('/') {
        Some((project, doc)) if doc.parse::<Uuid>().is_ok() => project,
        _ => key,
    }
}

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(list_documents, add_document))
}

/// What the list shows: the kinds stored as documents, and round proposals, which are
/// registered with their round instead.
#[derive(Debug, Clone, Copy, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(rename_all = "lowercase")]
enum ListedKind {
    Manifest,
    Notebook,
    Text,
    Proposal,
    Candidates,
}

impl From<DocumentKind> for ListedKind {
    fn from(kind: DocumentKind) -> Self {
        match kind {
            DocumentKind::Manifest => Self::Manifest,
            DocumentKind::Notebook => Self::Notebook,
            DocumentKind::Text => Self::Text,
        }
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct DocumentOut {
    id: Uuid,
    kind: ListedKind,
    created_by: Option<Uuid>,
    created_at: String,
}

impl From<DocumentRow> for DocumentOut {
    fn from(row: DocumentRow) -> Self {
        Self {
            id: row.id,
            kind: row.kind.into(),
            created_by: row.created_by,
            created_at: row.created_at,
        }
    }
}

impl From<ProjectProposal> for DocumentOut {
    fn from(proposal: ProjectProposal) -> Self {
        Self {
            id: proposal.doc,
            kind: if proposal.candidates {
                ListedKind::Candidates
            } else {
                ListedKind::Proposal
            },
            created_by: Some(proposal.author),
            created_at: proposal.created_at,
        }
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct NewDocument {
    /// `manifest` (the project's file list, once), then `notebook` or `text`.
    kind: DocumentKind,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct DocumentCreated {
    id: Uuid,
    kind: DocumentKind,
}

#[utoipa::path(
    get,
    path = "/api/rooms/{room_id}/documents",
    params(("room_id" = String, Path, description = "Project id (a room in the API)")),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The project's documents, oldest first, then the round proposals the caller may read, oldest first", body = [DocumentOut]),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such room, or the caller is not a member", body = Detail),
    )
)]
async fn list_documents(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
) -> Result<Json<Vec<DocumentOut>>, ApiError> {
    lifecycle::require_role(&state, &room_id, user.id, Role::Viewer).await?;
    let documents = db::document_rows(&state.pool, &room_id).await?;
    let proposals = readable_proposals(&state, &room_id, user.id).await?;
    Ok(Json(
        documents
            .into_iter()
            .map(DocumentOut::from)
            .chain(proposals.into_iter().map(DocumentOut::from))
            .collect(),
    ))
}

// simplify: asks the access rule once per proposal, reading its round and members each time;
// ceiling = a few hundred proposals per project; upgrade = load each round and its members once.
async fn readable_proposals(
    state: &AppState,
    room_id: &str,
    user: Uuid,
) -> Result<Vec<ProjectProposal>, ApiError> {
    let mut readable = Vec::new();
    for proposal in rounds::project_proposals(&state.pool, room_id).await? {
        let role = access::key_role(state, &key(room_id, proposal.doc), user).await?;
        if role.is_some() {
            readable.push(proposal);
        }
    }
    Ok(readable)
}

#[utoipa::path(
    post,
    path = "/api/rooms/{room_id}/documents",
    params(("room_id" = String, Path, description = "Project id (a room in the API)")),
    request_body = NewDocument,
    security(("cookie" = [])),
    responses(
        (status = 201, description = "The new, empty document", body = DocumentCreated),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller is a viewer", body = Detail),
        (status = 404, description = "No such room, or the caller is not a member", body = Detail),
        (status = 409, description = "The project already has its file list, or has none yet for a notebook or text", body = Detail),
        (status = 422, description = "The kind is none of manifest, notebook or text", body = Detail),
    )
)]
async fn add_document(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
    JsonBody(body): JsonBody<NewDocument>,
) -> Result<(StatusCode, Json<DocumentCreated>), ApiError> {
    let id = lifecycle::add_document(&state, &room_id, user.id, body.kind).await?;
    let created = DocumentCreated {
        id,
        kind: body.kind,
    };
    Ok((StatusCode::CREATED, Json(created)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_belongs_to_its_project_and_a_plain_id_to_itself() {
        let doc = Uuid::new_v4();

        assert_eq!(project_of(&key("lab", doc)), "lab");
        assert_eq!(project_of("lab"), "lab");
        assert_eq!(project_of("old/room"), "old/room");
        assert_eq!(
            project_of(&key(&key("a", doc), doc)),
            "a/".to_owned() + &doc.to_string()
        );
        let uuid_named = doc.to_string();
        assert_eq!(project_of(&key(&uuid_named, doc)), uuid_named);
    }
}
