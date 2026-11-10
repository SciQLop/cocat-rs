//! A workspace's documents: the manifest made with it, and the notebooks and texts added
//! later. Their content lives in sockets and storage, never in these routes.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use cocat_core::role::Role;
use serde::{Deserialize, Serialize};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::AppState;
use crate::api::{ApiError, ApiPath, CurrentUser, JsonBody};
use crate::db::{self, DocumentKind, DocumentRow};
use crate::lifecycle;
use crate::openapi::Detail;

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

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct DocumentOut {
    id: Uuid,
    kind: DocumentKind,
    created_by: Option<Uuid>,
    created_at: String,
}

impl From<DocumentRow> for DocumentOut {
    fn from(row: DocumentRow) -> Self {
        Self {
            id: row.id,
            kind: row.kind,
            created_by: row.created_by,
            created_at: row.created_at,
        }
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct NewDocument {
    /// `notebook` or `text`: the manifest is made with the workspace.
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
        (status = 200, description = "The workspace's documents, oldest first", body = [DocumentOut]),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such room, or the caller is not a member", body = Detail),
        (status = 409, description = "A catalogue project holds no documents", body = Detail),
    )
)]
async fn list_documents(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath(room_id): ApiPath<String>,
) -> Result<Json<Vec<DocumentOut>>, ApiError> {
    lifecycle::require_role(&state, &room_id, user.id, Role::Viewer).await?;
    lifecycle::require_workspace(&state, &room_id).await?;
    let documents = db::document_rows(&state.pool, &room_id).await?;
    Ok(Json(documents.into_iter().map(DocumentOut::from).collect()))
}

#[utoipa::path(
    post,
    path = "/api/rooms/{room_id}/documents",
    params(("room_id" = String, Path, description = "Project id (a room in the API)")),
    request_body = NewDocument,
    security(("cookie" = [])),
    responses(
        (status = 201, description = "The new, empty document", body = DocumentCreated),
        (status = 400, description = "A workspace has one manifest, made with it", body = Detail),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller is a viewer", body = Detail),
        (status = 404, description = "No such room, or the caller is not a member", body = Detail),
        (status = 409, description = "A catalogue project holds no documents", body = Detail),
        (status = 422, description = "The kind is neither notebook nor text", body = Detail),
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
    }
}
