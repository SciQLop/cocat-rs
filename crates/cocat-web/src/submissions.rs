//! Submissions to the public catalogues as the page shows them, and what its forms send.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::history::{Diff, browse_href};
use crate::public::PublicCatalogue;

/// A submission as `GET /api/public/submissions` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Submission {
    pub id: i64,
    pub kind: String,
    pub author: String,
    pub target: Option<String>,
    pub title: String,
    pub folder: Option<String>,
    pub description: Option<String>,
    pub note: Option<String>,
    pub status: String,
    pub reviewer: Option<String>,
    pub review_note: Option<String>,
    pub created: String,
    pub decided: Option<String>,
}

impl Submission {
    pub fn is_pending(&self) -> bool {
        self.status == "pending"
    }

    /// Who wrote the review note: a curator sees the reviewer's email, an author is only
    /// told it was a curator.
    pub fn said_by(&self) -> String {
        match self.reviewer.as_deref() {
            Some(email) if email.contains('@') => format!("{email} said"),
            _ => "The curator said".to_owned(),
        }
    }

    /// "New catalogue", or "Change to “Shocks”".
    pub fn kind_text(&self) -> String {
        match self.kind.as_str() {
            "update" => format!("Change to “{}”", self.title),
            _ => "New catalogue".to_owned(),
        }
    }

    pub fn status_text(&self) -> &'static str {
        match self.status.as_str() {
            "accepted" => "Accepted",
            "declined" => "Declined",
            "withdrawn" => "Withdrawn",
            _ => "Waiting for review",
        }
    }

    /// The day it was submitted, `2026-11-08`.
    pub fn day(&self) -> &str {
        self.created.get(..10).unwrap_or(&self.created)
    }

    /// The public catalogue to open: the one an update changes, or the one an accepted new
    /// submission became.
    pub fn public(&self) -> Option<&str> {
        self.target
            .as_deref()
            .filter(|_| self.kind == "update" || self.status == "accepted")
    }
}

/// One submission with what accepting it changes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Detail {
    #[serde(flatten)]
    pub submission: Submission,
    /// The public catalogue changed since the author copied it.
    pub stale: bool,
    pub changes: Diff,
}

/// A catalogue of the open project copied out of a public catalogue.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Proposal {
    pub catalogue: String,
    pub target: String,
    pub name: String,
}

pub fn pending(all: &[Submission]) -> usize {
    all.iter()
        .filter(|submission| submission.is_pending())
        .count()
}

/// What "Submit to Public catalogues" sends; a blank field is left to the server's default.
pub fn submit_body(room: &str, catalogue: &str, fields: &SubmitFields) -> Value {
    json!({
        "room": room,
        "catalogue": catalogue,
        "title": given(&fields.title),
        "folder": given(&fields.folder),
        "description": given(&fields.description),
        "note": given(&fields.note),
    })
}

/// What the submit form holds, as typed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubmitFields {
    pub title: String,
    pub folder: String,
    pub description: String,
    pub note: String,
}

pub fn update_body(room: &str, catalogue: &str, note: &str) -> Value {
    json!({ "room": room, "catalogue": catalogue, "note": given(note) })
}

pub fn review_body(accept: bool, note: &str) -> Value {
    json!({ "accept": accept, "note": given(note) })
}

/// Copies into `project`, or a new project when it is `None`.
pub fn propose_body(project: Option<&str>) -> Value {
    json!({ "room": project })
}

/// The folders the index uses, to suggest one.
pub fn folders(index: &[PublicCatalogue]) -> Vec<String> {
    let mut folders: Vec<String> = index
        .iter()
        .filter_map(|catalogue| catalogue.folder.clone())
        .collect();
    folders.sort();
    folders.dedup();
    folders
}

/// The copied catalogue in its project.
pub fn copy_href(room: &str, catalogue: &str) -> String {
    format!("{}#c={catalogue}", browse_href(room))
}

pub fn proposal_for<'a>(proposals: &'a [Proposal], catalogue: &str) -> Option<&'a Proposal> {
    proposals
        .iter()
        .find(|proposal| proposal.catalogue == catalogue)
}

fn given(text: &str) -> Option<&str> {
    Some(text.trim()).filter(|text| !text.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cocat_core::role::Role;

    fn submission(kind: &str, status: &str) -> Submission {
        Submission {
            id: 3,
            kind: kind.into(),
            author: "ada@example.org".into(),
            target: Some("public.shocks".into()),
            title: "Shocks".into(),
            folder: None,
            description: None,
            note: None,
            status: status.into(),
            reviewer: None,
            review_note: None,
            created: "2026-11-08T10:00:00Z".into(),
            decided: None,
        }
    }

    #[test]
    fn the_review_note_names_its_curator_only_by_email() {
        let mut decided = submission("new", "declined");
        assert_eq!(decided.said_by(), "The curator said");
        decided.reviewer = Some("a curator".into());
        assert_eq!(decided.said_by(), "The curator said");
        decided.reviewer = Some("alice@example.org".into());
        assert_eq!(decided.said_by(), "alice@example.org said");
    }

    #[test]
    fn a_submission_reads_in_plain_words() {
        let update = submission("update", "pending");
        assert_eq!(update.kind_text(), "Change to “Shocks”");
        assert_eq!(update.status_text(), "Waiting for review");
        assert_eq!(update.day(), "2026-11-08");
        assert_eq!(update.public(), Some("public.shocks"));
        let new = submission("new", "declined");
        assert_eq!(new.kind_text(), "New catalogue");
        assert_eq!(new.status_text(), "Declined");
        assert_eq!(new.public(), None, "a declined new one became nothing");
        assert_eq!(
            submission("new", "accepted").public(),
            Some("public.shocks")
        );
        assert_eq!(
            pending(&[update, new, submission("new", "withdrawn")]),
            1,
            "only those waiting count"
        );
    }

    #[test]
    fn a_detail_reads_the_server_answer() {
        let detail: Detail = serde_json::from_str(
            r#"{"id":1,"kind":"update","author":"a@b","target":"public.x","title":"X",
                "folder":null,"description":null,"note":"fixed","status":"pending",
                "reviewer":null,"review_note":null,"created":"2026-11-08T10:00:00Z",
                "decided":null,"stale":true,
                "changes":{"events":[{"uuid":"e","change":"added","fields":[],
                           "before":null,"after":{"uuid":"e"}}],"catalogues":[]}}"#,
        )
        .unwrap();
        assert!(detail.stale);
        assert_eq!(detail.submission.note.as_deref(), Some("fixed"));
        assert_eq!(detail.changes.events.len(), 1);
    }

    #[test]
    fn blank_fields_are_left_to_the_server() {
        let fields = SubmitFields {
            title: "  ".into(),
            folder: "Lab ".into(),
            description: String::new(),
            note: "please".into(),
        };
        assert_eq!(
            submit_body("r", "c", &fields),
            json!({"room": "r", "catalogue": "c", "title": null, "folder": "Lab",
                   "description": null, "note": "please"})
        );
        assert_eq!(
            update_body("r", "c", " "),
            json!({"room": "r", "catalogue": "c", "note": null})
        );
        assert_eq!(
            review_body(false, "needs sources"),
            json!({"accept": false, "note": "needs sources"})
        );
        assert_eq!(propose_body(None), json!({"room": null}));
        assert_eq!(propose_body(Some("lab")), json!({"room": "lab"}));
    }

    #[test]
    fn folders_are_suggested_once_each_in_order() {
        let entry = |folder: Option<&str>| PublicCatalogue {
            id: "public.x".into(),
            name: "x".into(),
            folder: folder.map(str::to_owned),
            source: None,
            description: None,
            role: Role::Viewer,
            changed: None,
            count: None,
        };
        let index = [
            entry(Some("B")),
            entry(None),
            entry(Some("A")),
            entry(Some("B")),
        ];
        assert_eq!(folders(&index), ["A", "B"]);
    }

    #[test]
    fn a_copy_opens_on_its_catalogue_and_a_link_is_found_by_catalogue() {
        assert_eq!(copy_href("lab", "c1"), "../rooms/lab/browse#c=c1");
        let proposals = [Proposal {
            catalogue: "c1".into(),
            target: "public.x".into(),
            name: "X".into(),
        }];
        assert_eq!(
            proposal_for(&proposals, "c1").map(|p| p.name.as_str()),
            Some("X")
        );
        assert_eq!(proposal_for(&proposals, "c2"), None);
    }
}
