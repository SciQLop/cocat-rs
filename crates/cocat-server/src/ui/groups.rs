//! The group pages: the list, one group with its members and invitations, and the join page.

use askama::Template;
use axum::Form;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use uuid::Uuid;

use crate::groups::{self, GroupRole};
use crate::links::segment;
use crate::ui::frame::{self, Flash, Frame, MaybeUser, RequireUser, ShareLink};
use crate::ui::{answer, query_value, redirect, redirect_to, token_from_text};
use crate::{AppState, db, invitations, lifecycle, mail};

/// Python's `INVITATION_LIFETIME`: every invitation this page makes lasts two weeks.
const INVITATION_DAYS: u32 = 14;

/// A caller who may act gets this for an id that is not a member, known or not.
const NOT_A_MEMBER: &str = "That user is not a member here.";
const INVALID: &str = "This invitation is not valid anymore";
const UNKNOWN_INVITER: &str = "Someone";

#[derive(Debug, Deserialize)]
pub(crate) struct NameForm {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct InviteForm {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    email: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TokenForm {
    #[serde(default)]
    token: String,
    #[serde(default)]
    link: String,
}

impl TokenForm {
    /// The hidden field carries the fragment; the visible box takes a whole pasted link.
    fn wanted(&self) -> String {
        let hidden = self.token.trim();
        if !hidden.is_empty() {
            return hidden.to_owned();
        }
        token_from_text(&self.link)
    }
}

struct LeaveForm {
    action: String,
    confirm: String,
}

struct GroupControls {
    rename_action: String,
    delete_action: String,
    delete_confirm: String,
}

struct GroupCard {
    id: String,
    name: String,
    href: String,
    role: &'static str,
    owner: Option<GroupControls>,
    leave: Option<LeaveForm>,
}

#[derive(Template)]
#[template(path = "groups.html")]
struct GroupsPage {
    frame: Frame,
    action: String,
    groups: Vec<GroupCard>,
}

struct MemberRemoval {
    action: String,
    confirm: String,
}

struct MemberLine {
    id: Uuid,
    email: String,
    role: &'static str,
    remove: Option<MemberRemoval>,
}

struct InvitationLine {
    id: i64,
    label: String,
    expires: String,
    usage: String,
    action: String,
}

struct InvitationPanel {
    action: String,
    share_link: Option<String>,
    suggestions: Vec<String>,
    invitations: Vec<InvitationLine>,
}

struct SharedRoom {
    name: String,
    href: String,
    role: &'static str,
}

#[derive(Template)]
#[template(path = "group.html")]
struct GroupPage {
    frame: Frame,
    name: String,
    role: &'static str,
    groups_href: String,
    leave: Option<LeaveForm>,
    members: Vec<MemberLine>,
    rooms: Vec<SharedRoom>,
    panel: Option<InvitationPanel>,
}

#[derive(Template)]
#[template(path = "group_not_found.html")]
struct NotFoundPage {
    frame: Frame,
    groups_href: String,
}

struct Offer {
    inviter: String,
    group_name: String,
    expires: String,
    token: String,
    signed_in: bool,
}

#[derive(Template)]
#[template(path = "group_join.html")]
struct JoinPage {
    frame: Frame,
    action: String,
    accept_action: String,
    paste: bool,
    submit_on_load: bool,
    message: &'static str,
    login_href: String,
    offer: Option<Offer>,
}

/// The list moved into the app; it stays for someone with no project the app could open.
pub(crate) async fn page(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Flash(flash): Flash,
) -> Response {
    if let Some(moved) = in_the_app(&state, &user, "groups").await {
        return moved;
    }
    match list_page(&state, &user, flash).await {
        Ok(page) => frame::page(page),
        Err(error) => {
            tracing::warn!(%error, "cannot read the group list");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Where a group page sends its visitor: the app at `fragment`, when it has a project to open.
async fn in_the_app(state: &AppState, user: &db::User, fragment: &str) -> Option<Response> {
    match super::app::entry(state, user).await {
        Ok(Some(target)) => Some(redirect_to(state, &format!("{target}#{fragment}"))),
        Ok(None) => None,
        Err(response) => Some(*response),
    }
}

async fn list_page(
    state: &AppState,
    user: &db::User,
    flash: Option<String>,
) -> Result<GroupsPage, lifecycle::LifecycleError> {
    let rows = groups::list_for(state, user.id).await?;
    Ok(GroupsPage {
        frame: Frame::new(&state.links, Some(user), "My groups").flashing(flash),
        action: state.links.path("/groups"),
        groups: rows
            .into_iter()
            .map(|(group, role)| card(state, group, role))
            .collect(),
    })
}

fn card(state: &AppState, group: groups::GroupRow, role: GroupRole) -> GroupCard {
    let owner = (role == GroupRole::Owner).then(|| GroupControls {
        rename_action: state.links.path(&format!("/groups/{}/rename", group.id)),
        delete_action: state.links.path(&format!("/groups/{}/delete", group.id)),
        delete_confirm: format!(
            "Delete {}? Its projects stop being shared with its members.",
            group.name
        ),
    });
    let leave = (role != GroupRole::Owner).then(|| LeaveForm {
        action: state.links.path(&format!("/groups/{}/leave", group.id)),
        confirm: format!("Leave {}?", group.name),
    });
    GroupCard {
        href: state.links.path(&format!("/groups/{}", group.id)),
        id: group.id,
        name: group.name,
        role: role.as_str(),
        owner,
        leave,
    }
}

pub(crate) async fn detail(
    Path(group_id): Path<String>,
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Flash(flash): Flash,
    ShareLink(link): ShareLink,
) -> Response {
    if groups::require_member(&state, &group_id, user.id)
        .await
        .is_ok()
        && let Some(moved) = in_the_app(&state, &user, &format!("group={group_id}")).await
    {
        return moved;
    }
    match detail_page(&state, &user, &group_id, flash, link).await {
        Ok(Some(page)) => frame::page(page),
        Ok(None) => not_found(&state, &user),
        Err(error) => {
            tracing::warn!(%error, "cannot read a group page");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn detail_page(
    state: &AppState,
    user: &db::User,
    group_id: &str,
    flash: Option<String>,
    share_link: Option<String>,
) -> Result<Option<GroupPage>, lifecycle::LifecycleError> {
    let Some((group, role)) = groups::list_for(state, user.id)
        .await?
        .into_iter()
        .find(|(group, _)| group.id == group_id)
    else {
        return Ok(None);
    };
    let members = members_of(state, &group.id, user.id, role).await?;
    let rooms = groups::shared_rooms(state, &group.id, user.id)
        .await?
        .into_iter()
        .map(|(room, role)| SharedRoom {
            href: state.links.path(&format!("/rooms/{}", segment(&room.id))),
            name: room.name,
            role: role.as_str(),
        })
        .collect();
    let panel = match role {
        GroupRole::Owner => {
            let emails: Vec<&str> = members.iter().map(|member| member.email.as_str()).collect();
            let suggestions = invitations::suggestions(state, user.id, &emails).await?;
            Some(panel_rows(state, &group.id, user.id, share_link, suggestions).await?)
        }
        GroupRole::Member => None,
    };
    let leave = (role != GroupRole::Owner).then(|| LeaveForm {
        action: state.links.path(&format!("/groups/{group_id}/leave")),
        confirm: format!("Leave {}?", group.name),
    });
    Ok(Some(GroupPage {
        frame: Frame::new(&state.links, Some(user), "Group").flashing(flash),
        name: group.name,
        role: role.as_str(),
        groups_href: state.links.path("/groups"),
        leave,
        members,
        rooms,
        panel,
    }))
}

async fn members_of(
    state: &AppState,
    group_id: &str,
    actor: Uuid,
    actor_role: GroupRole,
) -> Result<Vec<MemberLine>, lifecycle::LifecycleError> {
    let rows = groups::members(state, group_id, actor).await?;
    Ok(rows
        .into_iter()
        .map(|member| {
            let editable = actor_role == GroupRole::Owner && member.role != GroupRole::Owner;
            let remove = editable.then(|| MemberRemoval {
                action: state.links.path(&format!(
                    "/groups/{group_id}/members/{}/remove",
                    member.user_id
                )),
                confirm: format!("Remove {}?", member.email),
            });
            MemberLine {
                id: member.user_id,
                email: member.email,
                role: member.role.as_str(),
                remove,
            }
        })
        .collect())
}

async fn panel_rows(
    state: &AppState,
    group_id: &str,
    actor: Uuid,
    share_link: Option<String>,
    suggestions: Vec<String>,
) -> Result<InvitationPanel, lifecycle::LifecycleError> {
    let rows = groups::list_invitations(state, group_id, actor).await?;
    Ok(InvitationPanel {
        action: state.links.path(&format!("/groups/{group_id}/invitations")),
        share_link,
        suggestions,
        invitations: rows
            .into_iter()
            .map(|invitation| InvitationLine {
                label: invitation
                    .email
                    .clone()
                    .unwrap_or_else(|| "Share link".to_owned()),
                expires: invitation
                    .expires_at
                    .get(..10)
                    .unwrap_or(&invitation.expires_at)
                    .to_owned(),
                usage: usage(invitation.uses, invitation.max_uses),
                action: state.links.path(&format!(
                    "/groups/{group_id}/invitations/{}/revoke",
                    invitation.id
                )),
                id: invitation.id,
            })
            .collect(),
    })
}

fn usage(uses: i64, max_uses: Option<i64>) -> String {
    match max_uses {
        Some(max_uses) => format!("{uses}/{max_uses}"),
        None => String::new(),
    }
}

pub(crate) async fn create(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Form(form): Form<NameForm>,
) -> Response {
    let result = groups::create(&state, user.id, &form.name).await.map(drop);
    answer(&state, result, "/groups", "Group created".to_owned())
}

pub(crate) async fn rename(
    Path(group_id): Path<String>,
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Form(form): Form<NameForm>,
) -> Response {
    let result = groups::rename(&state, &group_id, user.id, &form.name)
        .await
        .map(drop);
    answer(&state, result, "/groups", "Group renamed".to_owned())
}

pub(crate) async fn delete(
    Path(group_id): Path<String>,
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
) -> Response {
    let result = groups::delete(&state, &group_id, user.id).await;
    answer(&state, result, "/groups", "Group deleted".to_owned())
}

pub(crate) async fn leave(
    Path(group_id): Path<String>,
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
) -> Response {
    let result = groups::remove_member(&state, &group_id, user.id, user.id).await;
    answer(&state, result, "/groups", "You left the group".to_owned())
}

pub(crate) async fn remove_member(
    Path((group_id, member)): Path<(String, Uuid)>,
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
) -> Response {
    let allowed = if member == user.id {
        groups::require_member(&state, &group_id, user.id)
            .await
            .map(|_| ())
    } else {
        groups::require_owner(&state, &group_id, user.id).await
    };
    if let Err(error) = allowed {
        return answer(&state, Err(error), "/groups", String::new());
    }
    let email = match member_answer(&state, &group_id, member).await {
        Ok(email) => email,
        Err(response) => return *response,
    };
    match groups::remove_member(&state, &group_id, user.id, member).await {
        Ok(()) => redirect(
            &state,
            &format!("/groups/{group_id}"),
            &format!("Removed {email}"),
        ),
        Err(error) => answer(&state, Err(error), "/groups", String::new()),
    }
}

pub(crate) async fn invite(
    Path(group_id): Path<String>,
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Form(form): Form<InviteForm>,
) -> Response {
    let target = format!("/groups/{group_id}");
    let address = form.email.trim();
    if form.kind == "link" || address.is_empty() {
        return match groups::create_invitation(
            &state,
            &group_id,
            user.id,
            None,
            INVITATION_DAYS,
            None,
        )
        .await
        {
            Ok((_, token)) if !token.is_empty() => {
                opened_link(&state, &target, "Share link created", &token)
            }
            Ok(_) => redirect(&state, &target, "Share link created"),
            Err(error) => answer(&state, Err(error), &target, String::new()),
        };
    }
    let Ok(address) = mail::Address::try_from(address.to_owned()) else {
        return redirect(&state, &target, "That is not an email address.");
    };
    match groups::create_invitation(
        &state,
        &group_id,
        user.id,
        Some(&address.0),
        INVITATION_DAYS,
        None,
    )
    .await
    {
        Ok((_, token)) if !token.is_empty() => {
            opened_link(&state, &target, "Invitation created", &token)
        }
        Ok(_) => redirect(
            &state,
            &target,
            &format!("Invitation sent to {}", address.0),
        ),
        Err(error) => answer(&state, Err(error), &target, String::new()),
    }
}

pub(crate) async fn revoke(
    Path((group_id, invitation)): Path<(String, i64)>,
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
) -> Response {
    let result = groups::revoke_invitation(&state, &group_id, user.id, invitation).await;
    answer(
        &state,
        result,
        &format!("/groups/{group_id}"),
        "Invitation revoked".to_owned(),
    )
}

pub(crate) async fn join_page(
    MaybeUser(user): MaybeUser,
    State(state): State<AppState>,
    Flash(flash): Flash,
) -> Response {
    frame::page(asking(&state, user.as_ref(), flash, ""))
}

pub(crate) async fn join_preview(
    MaybeUser(user): MaybeUser,
    State(state): State<AppState>,
    Form(form): Form<TokenForm>,
) -> Response {
    let wanted = form.wanted();
    if wanted.is_empty() {
        return frame::page(asking(&state, user.as_ref(), None, ""));
    }
    preview_page(&state, user.as_ref(), wanted).await
}

async fn preview_page(state: &AppState, user: Option<&db::User>, wanted: String) -> Response {
    match groups::preview_invitation(state, &wanted).await {
        Ok(preview) => frame::page(JoinPage {
            frame: Frame::new(&state.links, user, "Join a group"),
            action: state.links.path("/groups/join"),
            accept_action: state.links.path("/groups/join/accept"),
            paste: false,
            submit_on_load: false,
            message: "",
            login_href: login_href(state, user),
            offer: Some(Offer {
                inviter: preview
                    .inviter_email
                    .unwrap_or_else(|| UNKNOWN_INVITER.to_owned()),
                group_name: preview.group_name,
                expires: preview
                    .expires_at
                    .get(..10)
                    .unwrap_or(&preview.expires_at)
                    .to_owned(),
                token: wanted,
                signed_in: user.is_some(),
            }),
        }),
        Err(lifecycle::LifecycleError::NotFound(_)) => frame::page(refused(state, user, INVALID)),
        Err(error) => {
            tracing::warn!(%error, "cannot preview a group invitation");
            frame::page(refused(state, user, INVALID))
        }
    }
}

pub(crate) async fn join_accept(
    RequireUser(user): RequireUser,
    State(state): State<AppState>,
    Form(form): Form<TokenForm>,
) -> Response {
    let wanted = form.wanted();
    if wanted.is_empty() {
        return redirect_to(&state, "/groups/join");
    }
    match groups::accept_invitation(&state, &wanted, &user).await {
        Ok(group) => redirect(
            &state,
            &format!("/groups/{}", group.id),
            &format!("You joined {}", group.name),
        ),
        Err(error) => answer(&state, Err(error), "/groups/join", String::new()),
    }
}

fn asking(
    state: &AppState,
    user: Option<&db::User>,
    flash: Option<String>,
    message: &'static str,
) -> JoinPage {
    JoinPage {
        frame: Frame::new(&state.links, user, "Join a group").flashing(flash),
        action: state.links.path("/groups/join"),
        accept_action: state.links.path("/groups/join/accept"),
        paste: true,
        submit_on_load: true,
        message,
        login_href: login_href(state, user),
        offer: None,
    }
}

fn refused(state: &AppState, user: Option<&db::User>, message: &'static str) -> JoinPage {
    JoinPage {
        paste: false,
        submit_on_load: false,
        ..asking(state, user, None, message)
    }
}

fn login_href(state: &AppState, user: Option<&db::User>) -> String {
    if user.is_some() {
        return String::new();
    }
    format!(
        "{}?next={}",
        state.links.path("/login"),
        query_value(&state.links.path("/groups/join"))
    )
}

fn opened_link(state: &AppState, target: &str, notice: &str, token: &str) -> Response {
    let link = state.links.ui(&format!("/groups/join#{token}"));
    let response = frame::flash(&state.links, redirect_to(state, target), notice);
    frame::share_link(&state.links, response, &link)
}

async fn member_answer(
    state: &AppState,
    group_id: &str,
    member: Uuid,
) -> Result<String, Box<Response>> {
    match db::group_member_email(&state.pool, group_id, member).await {
        Ok(Some(email)) => Ok(email),
        Ok(None) => Err(Box::new(redirect(state, "/groups", NOT_A_MEMBER))),
        Err(error) => {
            tracing::warn!(%error, "cannot read a member");
            Err(Box::new(StatusCode::INTERNAL_SERVER_ERROR.into_response()))
        }
    }
}

fn not_found(state: &AppState, user: &db::User) -> Response {
    let page = NotFoundPage {
        frame: Frame::new(&state.links, Some(user), "Group not found"),
        groups_href: state.links.path("/groups"),
    };
    (StatusCode::NOT_FOUND, frame::page(page)).into_response()
}
