//! The project page a server without the catalogue browser keeps: members, groups and, for
//! whoever may share, invitations. With the browser installed, its Share dialog does this
//! instead.

use askama::Template;
use cocat_core::role::Role;
use uuid::Uuid;

use crate::links::segment;
use crate::ui::frame::Frame;
use crate::{AppState, db, groups, invitations, lifecycle};

struct LeaveForm {
    action: String,
    confirm: String,
}

struct MemberControls {
    role_action: String,
    remove_action: String,
    remove_confirm: String,
    transfer: Option<Transfer>,
}

struct Transfer {
    action: String,
    confirm: String,
}

/// The owner's switch that lets editors share.
struct SharingSwitch {
    action: String,
    allowed: bool,
}

struct MemberLine {
    id: String,
    email: String,
    role: &'static str,
    controls: Option<MemberControls>,
}

struct InvitationLine {
    id: i64,
    label: String,
    role: &'static str,
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

struct GroupOption {
    value: String,
    label: String,
}

struct GroupControls {
    action: String,
    remove_action: String,
    confirm: String,
}

struct GroupLine {
    id: String,
    name: String,
    role: &'static str,
    controls: Option<GroupControls>,
}

struct GroupPanel {
    action: String,
    picker: Vec<GroupOption>,
    groups: Vec<GroupLine>,
}

#[derive(Template)]
#[template(path = "room.html")]
pub(super) struct RoomPage {
    frame: Frame,
    name: String,
    role: &'static str,
    rooms_href: String,
    browse_href: String,
    leave: Option<LeaveForm>,
    members: Vec<MemberLine>,
    groups: GroupPanel,
    panel: Option<InvitationPanel>,
    switch: Option<SharingSwitch>,
    /// Who may share, said to someone who may not.
    sharers: String,
}

/// The page of `room`, which `user` reaches with `role`.
pub(super) async fn build(
    state: &AppState,
    user: &db::User,
    room: db::RoomRow,
    role: Role,
    flash: Option<String>,
    share_link: Option<String>,
) -> Result<RoomPage, lifecycle::LifecycleError> {
    let base = format!("/rooms/{}", segment(&room.id));
    let open = db::editors_can_share(&state.pool, &room.id)
        .await?
        .unwrap_or(false);
    let actor = Actor {
        role,
        sharer: role.may_share(open),
    };
    let members = members_of(state, &base, &room.id, actor).await?;
    let groups = groups_of(state, &base, user.id, &room.id, actor).await?;
    let panel = match actor.sharer {
        true => {
            let emails: Vec<&str> = members.iter().map(|member| member.email.as_str()).collect();
            let suggestions = invitations::suggestions(state, user.id, &emails).await?;
            Some(panel(state, &base, user.id, &room.id, share_link, suggestions).await?)
        }
        false => None,
    };
    let switch = (role == Role::Owner).then(|| SharingSwitch {
        action: state.links.path(&format!("{base}/editors-can-share")),
        allowed: open,
    });
    let leave = (role != Role::Owner).then(|| LeaveForm {
        action: state.links.path(&format!("{base}/leave")),
        confirm: format!("Leave {}?", room.name),
    });
    Ok(RoomPage {
        frame: Frame::new(&state.links, Some(user), "Project").flashing(flash),
        name: room.name,
        role: role.as_str(),
        rooms_href: state.links.path("/rooms"),
        browse_href: state.links.path(&format!("{base}/browse")),
        leave,
        members,
        groups,
        panel,
        switch,
        sharers: Role::sharers(open).to_owned(),
    })
}

/// What the visitor may do on the page.
#[derive(Clone, Copy)]
struct Actor {
    role: Role,
    sharer: bool,
}

async fn members_of(
    state: &AppState,
    base: &str,
    room_id: &str,
    actor: Actor,
) -> Result<Vec<MemberLine>, lifecycle::LifecycleError> {
    let rows = db::list_members(&state.pool, room_id).await?;
    Ok(rows
        .into_iter()
        .map(|(user, role)| {
            let editable = actor.sharer && role != Role::Owner;
            let member = format!("{base}/members/{}", user.id);
            let controls = editable.then(|| MemberControls {
                role_action: state.links.path(&format!("{member}/role")),
                remove_action: state.links.path(&format!("{member}/remove")),
                remove_confirm: format!("Remove {}?", user.email),
                transfer: (actor.role == Role::Owner).then(|| Transfer {
                    action: state.links.path(&format!("{member}/owner")),
                    confirm: format!("Make {} the owner?", user.email),
                }),
            });
            MemberLine {
                id: user.id.to_string(),
                email: user.email,
                role: role.as_str(),
                controls,
            }
        })
        .collect())
}

async fn groups_of(
    state: &AppState,
    base: &str,
    actor: Uuid,
    room_id: &str,
    visitor: Actor,
) -> Result<GroupPanel, lifecycle::LifecycleError> {
    let grants = groups::room_groups(state, room_id, actor).await?;
    let granted: Vec<&str> = grants.iter().map(|(group, _)| group.id.as_str()).collect();
    let picker = match visitor.sharer {
        true => groups::list_for(state, actor)
            .await?
            .into_iter()
            .filter(|(group, _)| !granted.contains(&group.id.as_str()))
            .map(|(group, _)| GroupOption {
                value: group.id,
                label: group.name,
            })
            .collect(),
        false => Vec::new(),
    };
    Ok(GroupPanel {
        action: state.links.path(&format!("{base}/groups")),
        picker,
        groups: grants
            .into_iter()
            .map(|(group, role)| group_line(state, base, group, role, visitor.sharer))
            .collect(),
    })
}

fn group_line(
    state: &AppState,
    base: &str,
    group: groups::GroupRow,
    role: Role,
    sharer: bool,
) -> GroupLine {
    let controls = sharer.then(|| GroupControls {
        action: state.links.path(&format!("{base}/groups")),
        remove_action: state
            .links
            .path(&format!("{base}/groups/{}/remove", segment(&group.id))),
        confirm: format!("Stop sharing with {}?", group.name),
    });
    GroupLine {
        id: group.id,
        name: group.name,
        role: role.as_str(),
        controls,
    }
}

async fn panel(
    state: &AppState,
    base: &str,
    actor: Uuid,
    room_id: &str,
    share_link: Option<String>,
    suggestions: Vec<String>,
) -> Result<InvitationPanel, lifecycle::LifecycleError> {
    let rows = invitations::list(state, room_id, actor).await?;
    Ok(InvitationPanel {
        action: state.links.path(&format!("{base}/invitations")),
        share_link,
        suggestions,
        invitations: rows
            .into_iter()
            .map(|invitation| InvitationLine {
                label: invitation
                    .email
                    .clone()
                    .unwrap_or_else(|| "Share link".to_owned()),
                role: invitation.role.as_str(),
                expires: invitation
                    .expires_at
                    .get(..10)
                    .unwrap_or(&invitation.expires_at)
                    .to_owned(),
                usage: usage(&invitation),
                action: state
                    .links
                    .path(&format!("{base}/invitations/{}/revoke", invitation.id)),
                id: invitation.id,
            })
            .collect(),
    })
}

fn usage(invitation: &invitations::Invitation) -> String {
    match invitation.max_uses {
        Some(max_uses) => format!("{}/{max_uses}", invitation.uses),
        None => String::new(),
    }
}
