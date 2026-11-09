//! Who a project or a group is shared with, and what the visitor may change there. The
//! rules are the server's: a project's owner, and its editors while the owner lets them,
//! change who is in it (`Role::may_share`); only a group's owner changes who is in the group.

use std::str::FromStr;

use cocat_core::role::Role;
use serde::{Deserialize, Deserializer, de};
use time::{Date, Month};
use url::Url;

use crate::presence::Peer;

/// A project member as `GET /api/rooms/{id}/members` lists them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Member {
    pub user_id: String,
    pub email: String,
    #[serde(deserialize_with = "role")]
    pub role: Role,
}

/// A group, as the visitor's group list or a project's grants name it, with the role: the
/// visitor's in the group ("owner" or "member"), or the one the group has in the project.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub role: String,
}

/// The `{"groups": [...]}` the group routes answer.
#[derive(Debug, Deserialize)]
pub struct Groups {
    pub groups: Vec<Group>,
}

/// A pending invitation; a group's carry no role.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Invitation {
    pub id: i64,
    pub email: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    pub expires_at: String,
    pub uses: i64,
}

/// What creating an invitation answers. The token is empty when the server mailed it.
#[derive(Debug, Clone, Deserialize)]
pub struct Created {
    pub token: String,
}

fn role<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Role, D::Error> {
    let name = String::deserialize(deserializer)?;
    Role::from_str(&name).map_err(de::Error::custom)
}

/// A member's row has a role picker and Remove: the visitor may share, and it is not the
/// owner.
pub fn member_editable(sharer: bool, member: Role) -> bool {
    sharer && member != Role::Owner
}

/// What the dialog tells someone who may not share: who may.
pub fn sharing_note(visitor: Role, editors_can_share: bool) -> Option<String> {
    (!visitor.may_share(editors_can_share)).then(|| {
        format!(
            "Only {} can invite people or change access.",
            Role::sharers(editors_can_share)
        )
    })
}

/// A group member's row has Remove: the visitor owns the group, and it is not the owner.
pub fn group_member_removable(visitor: &str, member: &str) -> bool {
    visitor == "owner" && member != "owner"
}

/// The groups of the visitor's that the project is not shared with yet.
pub fn addable(mine: &[Group], granted: &[Group]) -> Vec<Group> {
    mine.iter()
        .filter(|group| granted.iter().all(|grant| grant.id != group.id))
        .cloned()
        .collect()
}

/// The members as the Share dialog lists them: the owner first, then by address.
pub fn in_order(members: &[Member]) -> Vec<Member> {
    let mut members = members.to_vec();
    members
        .sort_by(|a, b| (a.role != Role::Owner, &a.email).cmp(&(b.role != Role::Owner, &b.email)));
    members
}

/// The members a project can be handed to: everyone but the visitor.
pub fn heirs<'a>(members: &'a [Member], visitor: &str) -> Vec<&'a Member> {
    members
        .iter()
        .filter(|member| member.email != visitor)
        .collect()
}

/// The colour of a member who is in the project right now.
pub fn colour_of<'a>(email: &str, peers: &'a [Peer]) -> Option<&'a str> {
    peers
        .iter()
        .find(|peer| peer.email == email)
        .map(|peer| peer.color.as_str())
}

/// One pending invitation, in words.
pub fn invitation_text(invitation: &Invitation, today: Date) -> String {
    let expires = expiry_text(&invitation.expires_at, today);
    let role = invitation
        .role
        .as_deref()
        .map(|role| format!(" as {role}"))
        .unwrap_or_default();
    match &invitation.email {
        Some(email) => format!("{email}, invited{role} · {expires}"),
        None => {
            let joined = match invitation.uses {
                1 => "1 person joined".to_owned(),
                uses => format!("{uses} people joined"),
            };
            format!("Share link{role} · {joined} · {expires}")
        }
    }
}

/// "expires in 12 days", from the server's `2026-11-22T10:00:00.000Z`.
pub fn expiry_text(expires_at: &str, today: Date) -> String {
    let Some(day) = date(expires_at) else {
        return format!("expires {expires_at}");
    };
    match (day - today).whole_days() {
        ..=0 => "expires today".to_owned(),
        1 => "expires tomorrow".to_owned(),
        days => format!("expires in {days} days"),
    }
}

fn date(stamp: &str) -> Option<Date> {
    let mut parts = stamp.get(..10)?.split('-');
    let year = parts.next()?.parse().ok()?;
    let month = Month::try_from(parts.next()?.parse::<u8>().ok()?).ok()?;
    let day = parts.next()?.parse().ok()?;
    Date::from_calendar_date(year, month, day).ok()
}

/// The page a project invitation opens, relative to the app's `<base>`.
pub const PROJECT_INVITATION: &str = "../invitation";
/// The page a group invitation opens.
pub const GROUP_INVITATION: &str = "../groups/join";

/// The link to hand out: the invitation page with the token in its fragment, so it never
/// reaches a server log.
pub fn invitation_link(base_uri: &str, page: &str, token: &str) -> Option<String> {
    let mut url = Url::parse(base_uri).ok()?.join(page).ok()?;
    url.set_fragment(Some(token));
    Some(url.into())
}

/// What an invitation is for: a project, whose invitations carry a role, or a group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub api: String,
    kind: &'static str,
    pub id: String,
}

impl Target {
    pub fn project(api: &str, id: &str) -> Self {
        Self {
            api: api.to_owned(),
            kind: "rooms",
            id: id.to_owned(),
        }
    }

    pub fn group(api: &str, id: &str) -> Self {
        Self {
            api: api.to_owned(),
            kind: "groups",
            id: id.to_owned(),
        }
    }

    /// The API path below the project or the group.
    pub fn path<'a>(&'a self, rest: &[&'a str]) -> Vec<&'a str> {
        [self.kind, self.id.as_str()]
            .into_iter()
            .chain(rest.iter().copied())
            .collect()
    }

    /// A project invitation grants a role; a group invitation makes a member.
    pub fn has_roles(&self) -> bool {
        self.kind == "rooms"
    }

    pub fn invitation_page(&self) -> &'static str {
        match self.has_roles() {
            true => PROJECT_INVITATION,
            false => GROUP_INVITATION,
        }
    }
}

/// What the invite form says once the server took the invitation.
pub fn invited_text(email: &str, mailed: bool) -> String {
    match mailed {
        true => format!("Invitation sent to {email}."),
        false => format!(
            "No mail is sent from this server: send {email} this link. It works once, for that address."
        ),
    }
}

#[cfg(test)]
mod tests {
    use time::macros::date;

    use super::*;

    #[test]
    fn the_owner_comes_first_then_addresses_in_order() {
        let member = |email: &str, role| Member {
            user_id: email.into(),
            email: email.into(),
            role,
        };
        let listed = [
            member("carol@x.org", Role::Viewer),
            member("zoe@x.org", Role::Owner),
            member("alice@x.org", Role::Editor),
        ];
        let emails: Vec<_> = in_order(&listed).into_iter().map(|m| m.email).collect();
        assert_eq!(emails, ["zoe@x.org", "alice@x.org", "carol@x.org"]);
    }

    fn member(email: &str, role: Role) -> Member {
        Member {
            user_id: format!("id-{email}"),
            email: email.into(),
            role,
        }
    }

    fn group(id: &str, role: &str) -> Group {
        Group {
            id: id.into(),
            name: format!("Group {id}"),
            role: role.into(),
        }
    }

    fn invitation(email: Option<&str>, uses: i64) -> Invitation {
        Invitation {
            id: 1,
            email: email.map(str::to_owned),
            role: Some("editor".into()),
            expires_at: "2026-11-20T10:00:00.000Z".into(),
            uses,
        }
    }

    #[test]
    fn the_api_lists_read_as_members_groups_and_invitations() {
        let members: Vec<Member> = serde_json::from_str(
            r#"[{"user_id":"u1","email":"a@x.org","role":"owner"},
                {"user_id":"u2","email":"b@x.org","role":"viewer"}]"#,
        )
        .unwrap();
        assert_eq!(members[1], member_with("u2", "b@x.org", Role::Viewer));
        let groups: Groups =
            serde_json::from_str(r#"{"groups":[{"id":"g","name":"Plasma team","role":"editor"}]}"#)
                .unwrap();
        assert_eq!(groups.groups[0].name, "Plasma team");
        let invitations: Vec<Invitation> = serde_json::from_str(
            r#"[{"id":3,"email":null,"expires_at":"2026-11-20T10:00:00.000Z","uses":2,"max_uses":null}]"#,
        )
        .unwrap();
        assert_eq!(invitations[0].role, None);
        assert_eq!(invitations[0].uses, 2);
    }

    fn member_with(id: &str, email: &str, role: Role) -> Member {
        Member {
            user_id: id.into(),
            email: email.into(),
            role,
        }
    }

    #[test]
    fn whoever_may_not_share_is_told_who_may() {
        assert_eq!(sharing_note(Role::Owner, false), None);
        assert_eq!(sharing_note(Role::Editor, true), None);
        assert_eq!(
            sharing_note(Role::Editor, false).as_deref(),
            Some("Only the owner can invite people or change access.")
        );
        assert_eq!(
            sharing_note(Role::Viewer, true).as_deref(),
            Some("Only the owner and editors can invite people or change access.")
        );
    }

    #[test]
    fn the_owner_row_never_has_controls() {
        assert!(member_editable(true, Role::Editor));
        assert!(member_editable(true, Role::Viewer));
        assert!(!member_editable(true, Role::Owner));
        for row in [Role::Owner, Role::Editor, Role::Viewer] {
            assert!(!member_editable(false, row), "{row:?}");
        }
    }

    #[test]
    fn only_a_group_owner_removes_members_and_never_themselves() {
        assert!(group_member_removable("owner", "member"));
        assert!(!group_member_removable("owner", "owner"));
        assert!(!group_member_removable("member", "member"));
    }

    #[test]
    fn the_group_picker_offers_only_groups_not_granted_yet() {
        let mine = [group("a", "owner"), group("b", "member")];
        let granted = [group("a", "viewer")];
        assert_eq!(addable(&mine, &granted), vec![group("b", "member")]);
    }

    #[test]
    fn ownership_goes_to_someone_else() {
        let members = [
            member("me@x.org", Role::Owner),
            member("b@x.org", Role::Editor),
        ];
        let heirs = heirs(&members, "me@x.org");
        assert_eq!(heirs, vec![&members[1]]);
    }

    #[test]
    fn a_member_who_is_here_shows_in_their_colour() {
        let peers = [Peer {
            email: "b@x.org".into(),
            initials: "B".into(),
            color: "#bf5300".into(),
            editing: None,
        }];
        assert_eq!(colour_of("b@x.org", &peers), Some("#bf5300"));
        assert_eq!(colour_of("c@x.org", &peers), None);
    }

    #[test]
    fn an_expiry_counts_whole_days() {
        let today = date!(2026 - 11 - 08);
        assert_eq!(
            expiry_text("2026-11-20T10:00:00.000Z", today),
            "expires in 12 days"
        );
        assert_eq!(
            expiry_text("2026-11-09T00:00:00Z", today),
            "expires tomorrow"
        );
        assert_eq!(expiry_text("2026-11-08T23:00:00Z", today), "expires today");
        assert_eq!(expiry_text("soon", today), "expires soon");
    }

    #[test]
    fn an_invitation_reads_as_who_it_is_for() {
        let today = date!(2026 - 11 - 08);
        assert_eq!(
            invitation_text(&invitation(Some("b@x.org"), 0), today),
            "b@x.org, invited as editor · expires in 12 days"
        );
        assert_eq!(
            invitation_text(&invitation(None, 1), today),
            "Share link as editor · 1 person joined · expires in 12 days"
        );
        let group_link = Invitation {
            role: None,
            ..invitation(None, 3)
        };
        assert_eq!(
            invitation_text(&group_link, today),
            "Share link · 3 people joined · expires in 12 days"
        );
    }

    #[test]
    fn an_invitation_link_keeps_its_token_in_the_fragment() {
        let base = "https://cocat.example.org/cocat-dev/ui/app/";
        assert_eq!(
            invitation_link(base, PROJECT_INVITATION, "t0k").unwrap(),
            "https://cocat.example.org/cocat-dev/ui/invitation#t0k"
        );
        assert_eq!(
            invitation_link(base, GROUP_INVITATION, "t0k").unwrap(),
            "https://cocat.example.org/cocat-dev/ui/groups/join#t0k"
        );
    }

    #[test]
    fn a_target_names_its_api_path_and_invitation_page() {
        let project = Target::project("https://h/api", "Bepi MSA");
        assert_eq!(
            project.path(&["invitations"]),
            ["rooms", "Bepi MSA", "invitations"]
        );
        assert!(project.has_roles());
        assert_eq!(project.invitation_page(), PROJECT_INVITATION);
        let group = Target::group("https://h/api", "g1");
        assert_eq!(group.path(&[]), ["groups", "g1"]);
        assert!(!group.has_roles());
        assert_eq!(group.invitation_page(), GROUP_INVITATION);
    }

    #[test]
    fn the_invite_form_says_whether_the_mail_left() {
        assert_eq!(invited_text("b@x.org", true), "Invitation sent to b@x.org.");
        assert!(invited_text("b@x.org", false).contains("send b@x.org this link"));
    }
}
