//! Managing a project: creating one, its members, its invitations and the groups granted on
//! it, through the server's JSON API; and the browser links to a project and an invitation.

use cocat_core::role::Role;
use cocat_core::room_id::path_segment;
use reqwest::{Method, RequestBuilder};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::login::{LoginError, RoomInfo, authorized, base, send};

/// How long an invitation stays valid when the caller does not say: the server's default.
pub const INVITATION_DAYS: u32 = 14;

/// A member of a project and the role they hold there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub user_id: String,
    pub email: String,
    pub role: Role,
}

/// What an invitation offers. `role` is sent as given: the server decides which roles an
/// invitation may grant.
#[derive(Clone, Debug, Serialize)]
pub struct NewInvitation<'a> {
    /// Only this address may accept it; without one, it is a link anyone may use.
    pub email: Option<&'a str>,
    pub role: &'a str,
    pub expires_in_days: u32,
    pub max_uses: Option<u32>,
}

/// A new invitation, with the page that accepts it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CreatedInvitation {
    pub id: i64,
    pub token: String,
    pub link: String,
}

/// A live invitation of a project, as the server lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invitation {
    pub id: i64,
    pub email: Option<String>,
    pub role: Role,
    pub expires_at: String,
    pub uses: i64,
    pub max_uses: Option<i64>,
}

/// A group granted on a project, with the role its members get there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectGroup {
    pub id: String,
    pub name: String,
    pub role: Role,
}

/// Makes a project owned by the caller.
pub async fn create_project(url: &str, cookie: &str, name: &str) -> Result<RoomInfo, LoginError> {
    #[derive(Serialize)]
    struct Body<'a> {
        name: &'a str,
    }
    #[derive(Deserialize)]
    struct Created {
        id: String,
        name: String,
        role: Role,
    }
    let request = api_request(Method::POST, url, cookie, &["rooms"])?.json(&Body { name });
    let created: Created = send(request).await?.json().await?;
    Ok(RoomInfo {
        id: created.id,
        name: created.name,
        role: created.role,
        personal: false,
        editors_can_share: true,
    })
}

/// A public catalogue in the server's index: every account reads it, curators edit it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicCatalogue {
    pub id: String,
    pub name: String,
    pub folder: Option<String>,
    pub source: Option<String>,
    pub description: Option<String>,
    pub role: Role,
    pub changed: Option<String>,
    /// How many events it holds outside the trash; `None` when the server could not count.
    pub count: Option<i64>,
}

/// What a new public catalogue says; `key` makes its id `public.<key>`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct NewPublicCatalogue<'a> {
    pub name: &'a str,
    pub folder: Option<&'a str>,
    pub source: Option<&'a str>,
    pub description: Option<&'a str>,
    pub key: Option<&'a str>,
}

pub async fn public_catalogues(
    url: &str,
    cookie: &str,
) -> Result<Vec<PublicCatalogue>, LoginError> {
    let request = api_request(Method::GET, url, cookie, &["public"])?;
    Ok(send(request).await?.json().await?)
}

/// What to change in a public catalogue's entry: `None` keeps a field, an empty string
/// clears it (the name cannot be cleared).
#[derive(Clone, Debug, Default, Serialize)]
pub struct PublicCatalogueChanges<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
}

/// Changes what the index says about a public catalogue; only curators may.
pub async fn update_public_catalogue(
    url: &str,
    cookie: &str,
    id: &str,
    changes: &PublicCatalogueChanges<'_>,
) -> Result<PublicCatalogue, LoginError> {
    let request = api_request(Method::PATCH, url, cookie, &["public", id])?.json(changes);
    Ok(send(request).await?.json().await?)
}

/// Makes a public catalogue; only curators may.
pub async fn create_public_catalogue(
    url: &str,
    cookie: &str,
    catalogue: &NewPublicCatalogue<'_>,
) -> Result<PublicCatalogue, LoginError> {
    let request = api_request(Method::POST, url, cookie, &["public"])?.json(catalogue);
    Ok(send(request).await?.json().await?)
}

pub async fn members(url: &str, cookie: &str, room_id: &str) -> Result<Vec<Member>, LoginError> {
    let request = room_request(Method::GET, url, cookie, room_id, &["members"])?;
    Ok(send(request).await?.json().await?)
}

/// Gives a member another role; `role` is sent as given, the server refuses what it does not
/// assign.
pub async fn set_role(
    url: &str,
    cookie: &str,
    room_id: &str,
    user_id: &str,
    role: &str,
) -> Result<(), LoginError> {
    #[derive(Serialize)]
    struct Body<'a> {
        role: &'a str,
    }
    let path = ["members", user_id];
    let request = room_request(Method::PATCH, url, cookie, room_id, &path)?.json(&Body { role });
    send(request).await?;
    Ok(())
}

pub async fn remove_member(
    url: &str,
    cookie: &str,
    room_id: &str,
    user_id: &str,
) -> Result<(), LoginError> {
    let request = room_request(Method::DELETE, url, cookie, room_id, &["members", user_id])?;
    send(request).await?;
    Ok(())
}

pub async fn invite(
    url: &str,
    cookie: &str,
    room_id: &str,
    invitation: &NewInvitation<'_>,
) -> Result<CreatedInvitation, LoginError> {
    #[derive(Deserialize)]
    struct Created {
        id: i64,
        token: String,
    }
    let request =
        room_request(Method::POST, url, cookie, room_id, &["invitations"])?.json(invitation);
    let created: Created = send(request).await?.json().await?;
    Ok(CreatedInvitation {
        link: invitation_link(url, &created.token)?,
        id: created.id,
        token: created.token,
    })
}

pub async fn invitations(
    url: &str,
    cookie: &str,
    room_id: &str,
) -> Result<Vec<Invitation>, LoginError> {
    let request = room_request(Method::GET, url, cookie, room_id, &["invitations"])?;
    Ok(send(request).await?.json().await?)
}

pub async fn revoke_invitation(
    url: &str,
    cookie: &str,
    room_id: &str,
    invitation_id: i64,
) -> Result<(), LoginError> {
    let id = invitation_id.to_string();
    let request = room_request(Method::DELETE, url, cookie, room_id, &["invitations", &id])?;
    send(request).await?;
    Ok(())
}

pub async fn project_groups(
    url: &str,
    cookie: &str,
    room_id: &str,
) -> Result<Vec<ProjectGroup>, LoginError> {
    #[derive(Deserialize)]
    struct Groups {
        groups: Vec<ProjectGroup>,
    }
    let request = room_request(Method::GET, url, cookie, room_id, &["groups"])?;
    let groups: Groups = send(request).await?.json().await?;
    Ok(groups.groups)
}

/// Lets the project's editors share it as its owner does, or keeps sharing to the owner.
/// Only the owner may.
pub async fn set_editors_can_share(
    url: &str,
    cookie: &str,
    room_id: &str,
    allowed: bool,
) -> Result<(), LoginError> {
    #[derive(Serialize)]
    struct Body {
        allowed: bool,
    }
    let request = room_request(Method::PUT, url, cookie, room_id, &["editors-can-share"])?
        .json(&Body { allowed });
    send(request).await?;
    Ok(())
}

/// The project's page in the web app, opening its Share dialog with `share`. The id is one
/// path segment, escaped as the server escapes it in its own links.
pub fn project_url(url: &str, room_id: &str, share: bool) -> Result<String, LoginError> {
    let mut page = base(url)?;
    let path = format!("{}ui/rooms/{}/browse", page.path(), path_segment(room_id));
    page.set_path(&path);
    page.set_fragment(share.then_some("share"));
    Ok(page.into())
}

/// The page that accepts an invitation, as the server writes it into its mails: the token
/// sits in the fragment, so it never reaches an access log.
fn invitation_link(url: &str, token: &str) -> Result<String, LoginError> {
    let mut page = base(url)?.join("ui/invitation")?;
    page.set_fragment(Some(token));
    Ok(page.into())
}

/// A request under `api/rooms/{room_id}`, each part its own path segment so a room id is
/// escaped rather than read as a path.
pub(crate) fn room_request(
    method: Method,
    url: &str,
    cookie: &str,
    room_id: &str,
    tail: &[&str],
) -> Result<RequestBuilder, LoginError> {
    let path: Vec<&str> = ["rooms", room_id]
        .into_iter()
        .chain(tail.iter().copied())
        .collect();
    api_request(method, url, cookie, &path)
}

pub(crate) fn api_request(
    method: Method,
    url: &str,
    cookie: &str,
    path: &[&str],
) -> Result<RequestBuilder, LoginError> {
    let request = reqwest::Client::new().request(method, api_url(url, path)?);
    Ok(authorized(request, cookie))
}

/// `api/` and the path under the server url, each part its own escaped segment.
pub(crate) fn api_url(url: &str, path: &[&str]) -> Result<Url, LoginError> {
    let mut url: Url = base(url)?;
    url.path_segments_mut()
        .map_err(|_| url::ParseError::RelativeUrlWithCannotBeABaseBase)?
        .pop_if_empty()
        .push("api")
        .extend(path);
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::{invitation_link, project_url};

    #[test]
    fn a_project_page_escapes_the_id_as_the_server_does() {
        assert_eq!(
            project_url("https://cocat.example.org/cocat-dev", "Bepi MSA?#%", false).unwrap(),
            "https://cocat.example.org/cocat-dev/ui/rooms/Bepi%20MSA%3F%23%25/browse"
        );
        assert_eq!(
            project_url("http://127.0.0.1:8000/", "a/b", true).unwrap(),
            "http://127.0.0.1:8000/ui/rooms/a%2Fb/browse#share"
        );
    }

    #[test]
    fn an_invitation_link_keeps_the_prefix_and_hides_the_token_in_the_fragment() {
        assert_eq!(
            invitation_link("https://cocat.example.org/cocat-dev", "t0k").unwrap(),
            "https://cocat.example.org/cocat-dev/ui/invitation#t0k"
        );
    }
}
