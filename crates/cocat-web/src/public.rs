//! The public catalogues' index: shelves by folder, narrowed by a search.

use cocat_core::role::Role;
use serde::Deserialize;

use crate::projects::{Project, role};

/// One public catalogue as `GET /api/public` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PublicCatalogue {
    pub id: String,
    pub name: String,
    pub folder: Option<String>,
    pub source: Option<String>,
    pub description: Option<String>,
    #[serde(deserialize_with = "role")]
    pub role: Role,
    pub changed: Option<String>,
    #[serde(default)]
    pub count: Option<i64>,
}

impl PublicCatalogue {
    /// The project the page switches to: nobody shares a public catalogue.
    pub fn project(&self) -> Project {
        Project {
            id: self.id.clone(),
            name: self.name.clone(),
            role: self.role,
            editors_can_share: false,
        }
    }

    /// The day it last changed, `2026-11-08`.
    pub fn day(&self) -> Option<&str> {
        self.changed.as_deref().and_then(|at| at.get(..10))
    }

    /// "12691 events", or nothing when the server did not count them.
    pub fn events(&self) -> Option<String> {
        let count = usize::try_from(self.count?).ok()?;
        Some(crate::stats::heading(count, count))
    }

    fn matches(&self, words: &[String]) -> bool {
        let text = [
            Some(self.name.as_str()),
            self.description.as_deref(),
            self.source.as_deref(),
            self.folder.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
        words.iter().all(|word| text.contains(word.as_str()))
    }
}

/// The catalogues of one folder; `None` holds those filed nowhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shelf {
    pub folder: Option<String>,
    pub catalogues: Vec<PublicCatalogue>,
}

impl Shelf {
    pub fn label(&self) -> String {
        match &self.folder {
            Some(folder) => folder
                .split('/')
                .map(str::trim)
                .collect::<Vec<_>>()
                .join(" / "),
            None => "Other".to_owned(),
        }
    }
}

/// The catalogues whose name, description, source or folder hold every word of `search`,
/// shelved by folder (unfiled last), each shelf by name.
pub fn shelves(all: &[PublicCatalogue], search: &str) -> Vec<Shelf> {
    let words: Vec<String> = search.split_whitespace().map(str::to_lowercase).collect();
    let mut kept: Vec<&PublicCatalogue> = all.iter().filter(|c| c.matches(&words)).collect();
    kept.sort_by_key(|c| {
        (
            c.folder.is_none(),
            c.folder.as_deref().map(str::to_lowercase),
            c.name.to_lowercase(),
        )
    });
    let mut shelves: Vec<Shelf> = Vec::new();
    for catalogue in kept {
        match shelves.last_mut() {
            Some(shelf) if shelf.folder == catalogue.folder => {
                shelf.catalogues.push(catalogue.clone())
            }
            _ => shelves.push(Shelf {
                folder: catalogue.folder.clone(),
                catalogues: vec![catalogue.clone()],
            }),
        }
    }
    shelves
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, folder: Option<&str>, description: Option<&str>) -> PublicCatalogue {
        PublicCatalogue {
            id: format!("public.{name}"),
            name: name.into(),
            folder: folder.map(str::to_owned),
            source: Some("AMDA".into()),
            description: description.map(str::to_owned),
            role: Role::Viewer,
            changed: Some("2026-11-08T10:00:00Z".into()),
            count: Some(1),
        }
    }

    fn names(shelves: &[Shelf]) -> Vec<(String, Vec<&str>)> {
        shelves
            .iter()
            .map(|shelf| {
                let names = shelf.catalogues.iter().map(|c| c.name.as_str()).collect();
                (shelf.label(), names)
            })
            .collect()
    }

    #[test]
    fn the_index_reads_as_public_catalogues() {
        let listed: Vec<PublicCatalogue> = serde_json::from_str(
            r#"[{"id":"public.a","name":"A","folder":null,"source":"AMDA",
                 "description":null,"role":"editor","changed":null}]"#,
        )
        .unwrap();
        assert_eq!(listed[0].role, Role::Editor);
        assert_eq!(listed[0].project().id, "public.a");
        assert!(!listed[0].project().editors_can_share);
        assert_eq!(listed[0].day(), None);
        assert_eq!(entry("x", None, None).day(), Some("2026-11-08"));
        assert_eq!(listed[0].events(), None, "an older server sends no count");
        assert_eq!(entry("x", None, None).events().as_deref(), Some("1 event"));
        let counted: PublicCatalogue = serde_json::from_str(
            r#"{"id":"public.b","name":"B","folder":null,"source":null,
                "description":null,"role":"viewer","changed":null,"count":12691}"#,
        )
        .unwrap();
        assert_eq!(counted.events().as_deref(), Some("12691 events"));
    }

    #[test]
    fn catalogues_are_shelved_by_folder_with_the_unfiled_last() {
        let all = [
            entry("shocks", Some("AMDA/Solar wind"), None),
            entry("loose", None, None),
            entry("cusps", Some("AMDA/Magnetosphere"), None),
            entry("CMEs", Some("AMDA/Solar wind"), None),
        ];
        assert_eq!(
            names(&shelves(&all, "")),
            vec![
                ("AMDA / Magnetosphere".to_owned(), vec!["cusps"]),
                ("AMDA / Solar wind".to_owned(), vec!["CMEs", "shocks"]),
                ("Other".to_owned(), vec!["loose"]),
            ]
        );
    }

    #[test]
    fn a_search_keeps_what_holds_every_word_in_any_field() {
        let all = [
            entry("shocks", Some("AMDA/Solar wind"), Some("Seen by ACE")),
            entry(
                "cusps",
                Some("AMDA/Magnetosphere"),
                Some("Cluster crossings"),
            ),
        ];
        assert_eq!(names(&shelves(&all, "ace")).len(), 1);
        assert_eq!(names(&shelves(&all, "  SOLAR  shocks ")).len(), 1);
        assert_eq!(names(&shelves(&all, "amda")).len(), 2);
        assert!(shelves(&all, "shocks cluster").is_empty());
    }
}
