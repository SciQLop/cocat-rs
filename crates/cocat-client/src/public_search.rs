//! The events of the public catalogues a smart catalogue's search finds. The server answers
//! (`GET /api/public/events`); the last answer of each search is kept on disk with its
//! version tag, so an unchanged answer is not sent again and a client offline still has it.

use std::path::{Path, PathBuf};

use cocat_core::model::EventRecord;
use cocat_core::rules::Predicate;
use reqwest::{Method, StatusCode, header};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::login::LoginError;
use crate::projects::api_request;

/// The most events one search brings; `total` says how many there are in all.
pub const PUBLIC_LIMIT: usize = 10_000;

/// The folder, in the client's cache folder, that keeps the answers.
const ANSWERS: &str = "public-searches";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublicSearch {
    pub total: usize,
    pub events: Vec<PublicEvent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublicEvent {
    /// The id of the public catalogue holding it.
    pub catalogue: String,
    pub event: EventRecord,
}

/// A search as the box writes it, or as a smart catalogue stores it.
#[derive(Debug, Clone, Copy)]
pub enum PublicQuery<'q> {
    Text(&'q str),
    Predicate(&'q Predicate),
}

impl PublicQuery<'_> {
    /// The query parameter the server reads it from.
    fn parameter(self) -> (&'static str, String) {
        match self {
            PublicQuery::Text(text) => ("q", text.to_owned()),
            PublicQuery::Predicate(predicate) => (
                "where",
                serde_json::to_string(predicate).unwrap_or_default(),
            ),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Kept {
    tag: String,
    answer: PublicSearch,
}

/// What `query` finds in the public catalogues. With `cache`, the answer kept there is sent
/// back when the server says it still holds, or when the server cannot be reached.
pub async fn search_public(
    url: &str,
    cookie: &str,
    query: PublicQuery<'_>,
    cache: Option<&Path>,
) -> Result<PublicSearch, LoginError> {
    let asked = query.parameter();
    let file = cache.map(|cache| answer_file(cache, &asked));
    let kept = file.as_deref().and_then(read_kept);
    match fetch(
        url,
        cookie,
        &asked,
        kept.as_ref().map(|kept| kept.tag.as_str()),
    )
    .await
    {
        Ok(Fetched::Unchanged) => kept
            .map(|kept| kept.answer)
            .ok_or_else(|| LoginError::Refused {
                status: 304,
                detail: "the server sent no answer".to_owned(),
            }),
        Ok(Fetched::New(fresh)) => {
            if let Some(file) = &file {
                write_kept(file, &fresh);
            }
            Ok(fresh.answer)
        }
        Err(LoginError::Http(error)) => match kept {
            Some(kept) => {
                tracing::info!("the public catalogues answer as last read: {error}");
                Ok(kept.answer)
            }
            None => Err(LoginError::Http(error)),
        },
        Err(error) => Err(error),
    }
}

enum Fetched {
    Unchanged,
    New(Kept),
}

async fn fetch(
    url: &str,
    cookie: &str,
    (name, value): &(&str, String),
    held: Option<&str>,
) -> Result<Fetched, LoginError> {
    let limit = PUBLIC_LIMIT.to_string();
    let mut request = api_request(Method::GET, url, cookie, &["public", "events"])?
        .query(&[(*name, value.as_str()), ("limit", limit.as_str())]);
    if let Some(held) = held {
        request = request.header(header::IF_NONE_MATCH, held);
    }
    let response = request.send().await?;
    if response.status() == StatusCode::NOT_MODIFIED {
        return Ok(Fetched::Unchanged);
    }
    let response = send_checked(response).await?;
    let tag = response
        .headers()
        .get(header::ETAG)
        .and_then(|tag| tag.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let answer = response.json().await?;
    Ok(Fetched::New(Kept { tag, answer }))
}

/// A refusal as the other calls report it.
async fn send_checked(response: reqwest::Response) -> Result<reqwest::Response, LoginError> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status().as_u16();
    let detail = response.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<serde_json::Value>(&detail)
        .ok()
        .and_then(|body| body.get("detail")?.as_str().map(str::to_owned))
        .unwrap_or(detail);
    Err(LoginError::Refused { status, detail })
}

fn answer_file(cache: &Path, (name, value): &(&str, String)) -> PathBuf {
    let name = format!("{:x}.json", Sha256::digest(format!("{name}={value}")));
    cache.join(ANSWERS).join(name)
}

fn read_kept(file: &Path) -> Option<Kept> {
    serde_json::from_slice(&std::fs::read(file).ok()?).ok()
}

/// A cache: a write that fails only costs the next download.
fn write_kept(file: &Path, kept: &Kept) {
    let written = file
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(file, serde_json::to_vec(kept).unwrap_or_default()));
    if let Err(error) = written {
        tracing::warn!("the public catalogues' answer was not kept: {error}");
    }
}
