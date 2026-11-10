//! Whole files of a workspace, stored by the SHA-256 of their content. A file is written once
//! per project: putting it again changes nothing.
//!
//! simplify: blobs are kept until the project is deleted (no sweep). Ceiling: a busy
//! workspace keeps every version it ever uploaded, bounded by the owner quota. Upgrade:
//! sweep the blobs no manifest entry and no history version within retention refers to.

use std::io;
use std::path::{Path, PathBuf};

use axum::body::{Body, Bytes, to_bytes};
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use cocat_core::role::Role;
use cocat_core::store::escaped_room_id;
use data_encoding::HEXLOWER;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::AppState;
use crate::api::{ApiError, ApiPath, CurrentUser};
use crate::db::{self, DbError};
use crate::lifecycle;
use crate::openapi::Detail;
use crate::submissions_api::{bytes, within_quota};

const FOLDER: &str = "blobs";

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(put_blob, get_blob))
}

/// The folder holding a project's files, named after the project the way its room file is.
fn folder(update_dir: &Path, room_id: &str) -> PathBuf {
    update_dir.join(FOLDER).join(escaped_room_id(room_id))
}

/// The bytes the project's stored files take.
pub async fn stored_bytes(pool: &SqlitePool, room_id: &str) -> Result<u64, DbError> {
    let total: i64 =
        sqlx::query_scalar("SELECT COALESCE(SUM(size), 0) FROM blob WHERE room_id = ?")
            .bind(room_id)
            .fetch_one(pool)
            .await?;
    Ok(u64::try_from(total).unwrap_or(0))
}

/// Removes a project's files from disk; the rows go with the project row.
pub async fn delete_folder(update_dir: &Path, room_id: &str) {
    let path = folder(update_dir, room_id);
    match tokio::fs::remove_dir_all(&path).await {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(room = %room_id, path = %path.display(), %error, "cannot delete a project's files");
        }
    }
}

fn valid_hash(hash: &str) -> Result<(), ApiError> {
    let lowercase_hex =
        hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    match lowercase_hex {
        true => Ok(()),
        false => Err(ApiError::BadRequest(
            "A file is named by the SHA-256 of its content: 64 lowercase hexadecimal characters"
                .to_owned(),
        )),
    }
}

async fn is_stored(pool: &SqlitePool, room_id: &str, hash: &str) -> Result<bool, DbError> {
    let found: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM blob WHERE room_id = ? AND sha256 = ?")
            .bind(room_id)
            .bind(hash)
            .fetch_optional(pool)
            .await?;
    Ok(found.is_some())
}

/// Writes next to the final name and renames, so a reader never sees half a file.
async fn write_file(dir: &Path, hash: &str, content: &[u8]) -> io::Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    let temporary = dir.join(format!(".{}.tmp", Uuid::new_v4().simple()));
    let written = tokio::fs::write(&temporary, content).await;
    let renamed = match written {
        Ok(()) => tokio::fs::rename(&temporary, dir.join(hash)).await,
        Err(error) => Err(error),
    };
    if renamed.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    renamed
}

async fn record(
    pool: &SqlitePool,
    room_id: &str,
    hash: &str,
    size: u64,
    author: Uuid,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT OR IGNORE INTO blob (room_id, sha256, size, created_by) VALUES (?, ?, ?, ?)",
    )
    .bind(room_id)
    .bind(hash)
    .bind(i64::try_from(size).unwrap_or(i64::MAX))
    .bind(author.to_string())
    .execute(pool)
    .await?;
    Ok(())
}

async fn read_body(body: Body, cap: u64) -> Result<Bytes, ApiError> {
    let limit = usize::try_from(cap).unwrap_or(usize::MAX);
    match to_bytes(body, limit).await {
        Ok(content) => Ok(content),
        Err(_) => Err(ApiError::TooLarge(format!(
            "This file is too big to store: a file holds at most {}",
            bytes(cap)
        ))),
    }
}

async fn owner_left(state: &AppState, room_id: &str) -> Result<u64, ApiError> {
    let owner = db::room_owner(&state.pool, room_id)
        .await?
        .ok_or(ApiError::Internal)?;
    Ok(state.rooms.owner_left(&state.pool, owner, None).await?)
}

#[utoipa::path(
    put,
    path = "/api/rooms/{room_id}/blobs/{sha256}",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("sha256" = String, Path, description = "SHA-256 of the body, 64 lowercase hexadecimal characters"),
    ),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    security(("cookie" = [])),
    responses(
        (status = 201, description = "The file is stored"),
        (status = 200, description = "The project already had this file"),
        (status = 400, description = "The name is not a SHA-256, or the body does not hash to it", body = Detail),
        (status = 401, description = "No valid session", body = Detail),
        (status = 403, description = "The caller is a viewer", body = Detail),
        (status = 404, description = "No such room, or the caller is not a member", body = Detail),
        (status = 409, description = "A catalogue project holds no files", body = Detail),
        (status = 413, description = "The file is over the file limit, or over what the owner's storage has left", body = Detail),
    )
)]
async fn put_blob(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, hash)): ApiPath<(String, String)>,
    body: Body,
) -> Result<StatusCode, ApiError> {
    lifecycle::require_role(&state, &room_id, user.id, Role::Editor).await?;
    lifecycle::require_workspace(&state, &room_id).await?;
    valid_hash(&hash)?;
    let content = read_body(body, state.rooms.limits().file).await?;
    if HEXLOWER.encode(&Sha256::digest(&content)) != hash {
        return Err(ApiError::BadRequest(
            "The body does not match the SHA-256 in the path".to_owned(),
        ));
    }
    if is_stored(&state.pool, &room_id, &hash).await? {
        return Ok(StatusCode::OK);
    }
    let size = content.len() as u64;
    // simplify: measure, then write, so concurrent uploads can overshoot the owner's quota by one
    // file each. Upgrade path: reserve the bytes under a lock or in a database counter.
    let left = owner_left(&state, &room_id).await?;
    within_quota(size, left, "The project owner's", "file")?;
    write_file(&folder(state.rooms.update_dir(), &room_id), &hash, &content)
        .await
        .map_err(|error| {
            tracing::error!(%error, room = %room_id, "cannot store a file");
            ApiError::Internal
        })?;
    record(&state.pool, &room_id, &hash, size, user.id).await?;
    Ok(StatusCode::CREATED)
}

#[utoipa::path(
    get,
    path = "/api/rooms/{room_id}/blobs/{sha256}",
    params(
        ("room_id" = String, Path, description = "Project id (a room in the API)"),
        ("sha256" = String, Path, description = "SHA-256 of the file, 64 lowercase hexadecimal characters"),
    ),
    security(("cookie" = [])),
    responses(
        (status = 200, description = "The file's bytes", content_type = "application/octet-stream", body = Vec<u8>),
        (status = 400, description = "The name is not a SHA-256", body = Detail),
        (status = 401, description = "No valid session", body = Detail),
        (status = 404, description = "No such room, the caller is not a member, or the project does not hold this file", body = Detail),
        (status = 409, description = "A catalogue project holds no files", body = Detail),
    )
)]
async fn get_blob(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    ApiPath((room_id, hash)): ApiPath<(String, String)>,
) -> Result<Response, ApiError> {
    lifecycle::require_role(&state, &room_id, user.id, Role::Viewer).await?;
    lifecycle::require_workspace(&state, &room_id).await?;
    valid_hash(&hash)?;
    let missing = || ApiError::NotFound(format!("Project {room_id} holds no file {hash}"));
    if !is_stored(&state.pool, &room_id, &hash).await? {
        return Err(missing());
    }
    let path = folder(state.rooms.update_dir(), &room_id).join(&hash);
    let content = tokio::fs::read(&path).await.map_err(|error| {
        tracing::error!(%error, path = %path.display(), "cannot read a stored file");
        ApiError::Internal
    })?;
    let headers = [(header::CONTENT_TYPE, "application/octet-stream")];
    Ok((headers, content).into_response())
}
