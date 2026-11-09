//! Submissions to the public catalogues: offering a catalogue, copying a public one to
//! propose a change or to use it, and the curators' review.

use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::login::{LoginError, send};
use crate::projects::api_request;

/// A submission as its author and the curators see it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submission {
    pub id: i64,
    /// `new`, or `update` for a change to a public catalogue.
    pub kind: String,
    /// The author's email.
    pub author: String,
    /// The public catalogue an update changes, or the one an accepted new one became.
    pub target: Option<String>,
    pub title: String,
    pub folder: Option<String>,
    pub description: Option<String>,
    pub note: Option<String>,
    /// `pending`, `accepted`, `declined` or `withdrawn`.
    pub status: String,
    pub reviewer: Option<String>,
    pub review_note: Option<String>,
    pub created: String,
    pub decided: Option<String>,
}

/// One of the caller's catalogues offered as a new public catalogue. The title defaults to
/// the catalogue's name.
#[derive(Clone, Debug, Default, Serialize)]
pub struct NewSubmission<'a> {
    #[serde(rename = "room")]
    pub project_id: &'a str,
    pub catalogue: &'a str,
    pub title: Option<&'a str>,
    pub folder: Option<&'a str>,
    pub description: Option<&'a str>,
    pub note: Option<&'a str>,
}

/// Where a copy of a public catalogue went.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Copied {
    #[serde(rename(deserialize = "room"))]
    pub project_id: String,
    /// The project's name.
    pub name: String,
    /// The copied catalogue's uuid, the public one's.
    pub catalogue: String,
}

pub async fn submit_catalogue(
    url: &str,
    cookie: &str,
    submission: &NewSubmission<'_>,
) -> Result<Submission, LoginError> {
    let request = api_request(Method::POST, url, cookie, &["public", "submissions"])?;
    Ok(send(request.json(submission)).await?.json().await?)
}

/// Copies the public catalogue into `project_id`, or a new project (named `name`), to
/// change it there and submit it as an update.
pub async fn propose_change(
    url: &str,
    cookie: &str,
    public_id: &str,
    project_id: Option<&str>,
    name: Option<&str>,
) -> Result<Copied, LoginError> {
    #[derive(Serialize)]
    struct Body<'a> {
        room: Option<&'a str>,
        name: Option<&'a str>,
    }
    let body = Body {
        room: project_id,
        name,
    };
    let path = ["public", public_id, "proposals"];
    let request = api_request(Method::POST, url, cookie, &path)?.json(&body);
    Ok(send(request).await?.json().await?)
}

/// Copies the public catalogue, keeping its name, into `project_id` or a new project, to
/// use it there; unlike [`propose_change`], the copy is not offered back.
pub async fn copy_public_catalogue(
    url: &str,
    cookie: &str,
    public_id: &str,
    project_id: Option<&str>,
) -> Result<Copied, LoginError> {
    #[derive(Serialize)]
    struct Body<'a> {
        room: Option<&'a str>,
    }
    let path = ["public", public_id, "copies"];
    let request = api_request(Method::POST, url, cookie, &path)?.json(&Body { room: project_id });
    Ok(send(request).await?.json().await?)
}

/// Offers a copy made by [`propose_change`] as the public catalogue's next version.
pub async fn submit_update(
    url: &str,
    cookie: &str,
    project_id: &str,
    catalogue: &str,
    note: Option<&str>,
) -> Result<Submission, LoginError> {
    #[derive(Serialize)]
    struct Body<'a> {
        room: &'a str,
        catalogue: &'a str,
        note: Option<&'a str>,
    }
    let body = Body {
        room: project_id,
        catalogue,
        note,
    };
    let path = ["public", "submissions", "updates"];
    let request = api_request(Method::POST, url, cookie, &path)?.json(&body);
    Ok(send(request).await?.json().await?)
}

/// The caller's own submissions; for a curator without `mine`, everyone's.
pub async fn submissions(
    url: &str,
    cookie: &str,
    mine: bool,
) -> Result<Vec<Submission>, LoginError> {
    let request =
        api_request(Method::GET, url, cookie, &["public", "submissions"])?.query(&[("mine", mine)]);
    Ok(send(request).await?.json().await?)
}

/// A curator accepts (the public catalogue takes it) or declines a pending submission.
pub async fn review_submission(
    url: &str,
    cookie: &str,
    id: i64,
    accept: bool,
    note: Option<&str>,
) -> Result<Submission, LoginError> {
    #[derive(Serialize)]
    struct Body<'a> {
        accept: bool,
        note: Option<&'a str>,
    }
    let id = id.to_string();
    let path = ["public", "submissions", id.as_str(), "review"];
    let request = api_request(Method::POST, url, cookie, &path)?.json(&Body { accept, note });
    Ok(send(request).await?.json().await?)
}

/// Its author takes back a pending submission.
pub async fn withdraw_submission(
    url: &str,
    cookie: &str,
    id: i64,
) -> Result<Submission, LoginError> {
    let id = id.to_string();
    let path = ["public", "submissions", id.as_str(), "withdraw"];
    let request = api_request(Method::POST, url, cookie, &path)?;
    Ok(send(request).await?.json().await?)
}
