//! The home page as the page shows it: what `GET /api/me/home` answers, and its lines.

use serde::Deserialize;

use crate::history::{Version, author, counts_text};
use crate::submissions::Submission;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Home {
    pub invitations: Vec<Invitation>,
    pub reviews: Vec<Submission>,
    pub submissions: Vec<Submission>,
    pub projects: Vec<ProjectNews>,
}

/// An invitation addressed to the visitor's email.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Invitation {
    pub id: i64,
    pub room_id: String,
    pub room_name: String,
    pub role: String,
    pub inviter_email: Option<String>,
}

/// One project, and what others changed in it since the visitor last opened it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ProjectNews {
    pub id: String,
    pub name: String,
    pub role: String,
    pub last_change_ms: Option<i64>,
    pub changes: Vec<Version>,
    pub more: usize,
}

impl Home {
    /// Whether anything waits on the visitor.
    pub fn waiting(&self) -> bool {
        !(self.invitations.is_empty() && self.reviews.is_empty() && self.submissions.is_empty())
    }

    /// The projects with news first, as the server ordered them, then the quiet ones.
    pub fn split(&self) -> (Vec<ProjectNews>, Vec<ProjectNews>) {
        self.projects
            .iter()
            .cloned()
            .partition(|project| !project.changes.is_empty())
    }
}

/// "Alice invited you as an editor", or "You are invited as a viewer".
pub fn invitation_text(invitation: &Invitation) -> String {
    let article = match invitation.role.as_str() {
        "editor" => "an",
        _ => "a",
    };
    match &invitation.inviter_email {
        Some(email) => format!("{email} invited you as {article} {}", invitation.role),
        None => format!("You are invited as {article} {}", invitation.role),
    }
}

/// A version in one line for the home page: "3 events added, 1 changed in 2 catalogues",
/// "Rule edited", "Restored an earlier version", "Imported from tscat".
pub fn change_text(version: &Version) -> String {
    if version.restored_from.is_some() {
        return "Restored an earlier version".to_owned();
    }
    if let Some(origin) = version
        .auto_name
        .as_deref()
        .and_then(|name| name.strip_prefix("import: "))
    {
        return format!("Imported from {origin}");
    }
    let rule = (!version.rules_changed.is_empty()).then(|| "Rule edited".to_owned());
    let counts = event_counts(version)
        .map(|counts| format!("{counts}{}", in_catalogues(version.catalogues.len())));
    let parts: Vec<String> = counts.into_iter().chain(rule).collect();
    match parts.is_empty() {
        true => "Catalogues changed".to_owned(),
        false => parts.join(", "),
    }
}

/// "3 events added, 1 changed", the first count naming what is counted.
fn event_counts(version: &Version) -> Option<String> {
    let counts = counts_text(
        version.events_added,
        version.events_changed,
        version.events_removed,
    )?;
    let (count, rest) = counts.split_once(' ')?;
    let noun = if count == "1" { "event" } else { "events" };
    Some(format!("{count} {noun} {rest}"))
}

fn in_catalogues(count: usize) -> String {
    match count {
        0 | 1 => String::new(),
        count => format!(" in {count} catalogues"),
    }
}

/// Who made it, as the history names them.
pub fn who(version: &Version) -> String {
    author(version)
}

/// "5 min ago", "3 h ago", "yesterday", "4 days ago"; a date beyond a week.
pub fn ago(then_ms: i64, now_ms: i64, date: impl Fn(i64) -> String) -> String {
    const MINUTE: i64 = 60_000;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    match now_ms.saturating_sub(then_ms) {
        span if span < MINUTE => "just now".to_owned(),
        span if span < HOUR => format!("{} min ago", span / MINUTE),
        span if span < DAY => format!("{} h ago", span / HOUR),
        span if span < 2 * DAY => "yesterday".to_owned(),
        span if span < 7 * DAY => format!("{} days ago", span / DAY),
        _ => date(then_ms),
    }
}

/// "and 3 more changes" under a project's last five.
pub fn more_text(more: usize) -> Option<String> {
    match more {
        0 => None,
        1 => Some("and 1 more change".to_owned()),
        more => Some(format!("and {more} more changes")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWER: &str = r##"{
      "invitations": [
        {"id": 7, "room_id": "lab", "room_name": "Lab", "role": "viewer",
         "inviter_email": "alice@x.org", "expires_at": "2026-11-23T10:00:00.000Z"}
      ],
      "reviews": [],
      "submissions": [
        {"id": 3, "kind": "new", "author": "bob@x.org", "target": null, "title": "Shocks 2019",
         "folder": null, "description": null, "note": null, "status": "declined",
         "reviewer": "a curator", "review_note": "duplicates Shocks",
         "created": "2026-11-01T09:00:00Z", "decided": "2026-11-02T09:00:00Z"}
      ],
      "projects": [
        {"id": "lab", "name": "Lab", "role": "owner",
         "last_change_ms": 1762680000000, "seen_ms": 1762590000000,
         "changes": [
           {"id": 42, "last": 44, "author": "6f1c", "started_ms": 1762679000000,
            "ended_ms": 1762680000000, "via": null, "auto_name": null,
            "events_added": 3, "events_changed": 1, "events_removed": 0,
            "catalogues": ["c1", "c2"], "rules_changed": [], "restored_from": null,
            "restored_from_ms": null, "author_email": "bob@x.org", "author_color": "#1b7f72",
            "name": null, "committed": false, "message": null}
         ],
         "more": 2},
        {"id": "quiet", "name": "Quiet", "role": "owner", "last_change_ms": null,
         "seen_ms": null, "changes": [], "more": 0}
      ]
    }"##;

    fn home() -> Home {
        serde_json::from_str(ANSWER).unwrap()
    }

    #[test]
    fn the_servers_answer_reads() {
        let home = home();
        assert_eq!(home.invitations[0].room_name, "Lab");
        assert_eq!(home.submissions[0].status, "declined");
        assert_eq!(home.projects[0].more, 2);
        assert!(home.waiting());
    }

    #[test]
    fn projects_with_news_come_apart_from_the_quiet_ones() {
        let (news, quiet) = home().split();
        assert_eq!(
            news.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            ["lab"]
        );
        assert_eq!(
            quiet.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            ["quiet"]
        );
    }

    #[test]
    fn nothing_waits_when_every_list_is_empty() {
        let mut home = home();
        home.invitations.clear();
        home.submissions.clear();
        assert!(!home.waiting());
    }

    #[test]
    fn an_invitation_names_who_sent_it() {
        let mut invitation = home().invitations[0].clone();
        assert_eq!(
            invitation_text(&invitation),
            "alice@x.org invited you as a viewer"
        );
        invitation.role = "editor".into();
        invitation.inviter_email = None;
        assert_eq!(invitation_text(&invitation), "You are invited as an editor");
    }

    #[test]
    fn a_change_says_what_it_did_in_plain_words() {
        let mut version = home().projects[0].changes[0].clone();
        assert_eq!(
            change_text(&version),
            "3 events added, 1 changed in 2 catalogues"
        );
        version.catalogues = vec!["c1".into()];
        version.rules_changed = vec!["c1".into()];
        assert_eq!(
            change_text(&version),
            "3 events added, 1 changed, Rule edited"
        );
        version.events_added = 0;
        version.rules_changed.clear();
        assert_eq!(change_text(&version), "1 event changed");
        version.restored_from = Some(3);
        assert_eq!(change_text(&version), "Restored an earlier version");
        assert_eq!(who(&version), "bob");
    }

    #[test]
    fn time_reads_as_how_long_ago() {
        let now = 10 * 24 * 3_600_000;
        let date = |_| "1 Nov".to_owned();
        assert_eq!(ago(now - 20_000, now, date), "just now");
        assert_eq!(ago(now - 5 * 60_000, now, date), "5 min ago");
        assert_eq!(ago(now - 3 * 3_600_000, now, date), "3 h ago");
        assert_eq!(ago(now - 30 * 3_600_000, now, date), "yesterday");
        assert_eq!(ago(now - 4 * 24 * 3_600_000, now, date), "4 days ago");
        assert_eq!(ago(now - 9 * 24 * 3_600_000, now, date), "1 Nov");
    }

    #[test]
    fn more_changes_are_counted() {
        assert_eq!(more_text(0), None);
        assert_eq!(more_text(1).unwrap(), "and 1 more change");
        assert_eq!(more_text(4).unwrap(), "and 4 more changes");
    }
}
