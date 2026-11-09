//! The public catalogues' index: a folder tree, and shelves by folder narrowed by a search.

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
            personal: false,
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

    /// Its folder as the tree names it, `AMDA/EARTH`; `None` when filed nowhere.
    pub fn folder_path(&self) -> Option<String> {
        let parts = folder_parts(self.folder.as_deref()?);
        (!parts.is_empty()).then(|| parts.join("/"))
    }

    /// Filed in the folder `path` or in one of its subfolders.
    pub fn in_folder(&self, path: &str) -> bool {
        self.folder_path().is_some_and(|mine| {
            mine == path
                || mine
                    .strip_prefix(path)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
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

/// The catalogues of one folder (by its path, `AMDA/EARTH`); `None` holds those filed nowhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shelf {
    pub folder: Option<String>,
    pub catalogues: Vec<PublicCatalogue>,
}

impl Shelf {
    pub fn label(&self) -> String {
        match &self.folder {
            Some(folder) => folder_label(folder),
            None => "Other".to_owned(),
        }
    }
}

/// One folder of the tree, with how many catalogues it and its subfolders hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub name: String,
    /// `AMDA/EARTH`: what the URL and [`PublicCatalogue::in_folder`] name it by.
    pub path: String,
    pub count: usize,
    pub children: Vec<Folder>,
}

/// `AMDA / EARTH/` → `["AMDA", "EARTH"]`.
fn folder_parts(folder: &str) -> Vec<&str> {
    folder
        .split('/')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect()
}

/// The folders the catalogues are filed in, nested by their `/` parts, each level by name.
pub fn folder_tree(all: &[PublicCatalogue]) -> Vec<Folder> {
    let mut tree = Vec::new();
    for path in all.iter().filter_map(PublicCatalogue::folder_path) {
        file(&mut tree, &path, None);
    }
    sorted(tree)
}

fn file(level: &mut Vec<Folder>, path: &str, parent: Option<&str>) {
    let (name, rest) = path.split_once('/').unwrap_or((path, ""));
    let here = parent.map_or_else(|| name.to_owned(), |parent| format!("{parent}/{name}"));
    let index = match level.iter().position(|folder| folder.name == name) {
        Some(index) => index,
        None => {
            level.push(Folder {
                name: name.to_owned(),
                path: here.clone(),
                count: 0,
                children: Vec::new(),
            });
            level.len() - 1
        }
    };
    let folder = &mut level[index];
    folder.count += 1;
    if !rest.is_empty() {
        file(&mut folder.children, rest, Some(&here));
    }
}

fn sorted(mut level: Vec<Folder>) -> Vec<Folder> {
    level.sort_by_key(|folder| (folder.name.to_lowercase(), folder.name.clone()));
    level
        .into_iter()
        .map(|folder| Folder {
            children: sorted(folder.children),
            ..folder
        })
        .collect()
}

/// The folder `path` and the folders above it, which the tree shows open.
pub fn open_folders(path: &str) -> Vec<String> {
    let parts = folder_parts(path);
    (1..=parts.len()).map(|n| parts[..n].join("/")).collect()
}

/// What the index lists: the catalogues of `folder` (all of them when `None`) that hold
/// every word of `search`, shelved.
pub fn index(all: &[PublicCatalogue], folder: Option<&str>, search: &str) -> Vec<Shelf> {
    match folder {
        None => shelves(all, search),
        Some(path) => {
            let scoped: Vec<PublicCatalogue> = all
                .iter()
                .filter(|catalogue| catalogue.in_folder(path))
                .cloned()
                .collect();
            shelves(&scoped, search)
        }
    }
}

/// The folder a label names in the page's words: `AMDA / EARTH`.
pub fn folder_label(path: &str) -> String {
    folder_parts(path).join(" / ")
}

/// The catalogues whose name, description, source or folder hold every word of `search`,
/// shelved by folder (unfiled last), each shelf by name.
pub fn shelves(all: &[PublicCatalogue], search: &str) -> Vec<Shelf> {
    let words: Vec<String> = search.split_whitespace().map(str::to_lowercase).collect();
    let mut kept: Vec<&PublicCatalogue> = all.iter().filter(|c| c.matches(&words)).collect();
    kept.sort_by_key(|c| {
        let folder = c.folder_path();
        (
            folder.is_none(),
            folder.map(|path| path.to_lowercase()),
            c.name.to_lowercase(),
        )
    });
    let mut shelves: Vec<Shelf> = Vec::new();
    for catalogue in kept {
        let folder = catalogue.folder_path();
        match shelves.last_mut() {
            Some(shelf) if shelf.folder == folder => shelf.catalogues.push(catalogue.clone()),
            _ => shelves.push(Shelf {
                folder,
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

    /// A folder's path and count.
    type Counted = (String, usize);

    fn folders(tree: &[Folder]) -> Vec<(String, usize, Vec<Counted>)> {
        tree.iter()
            .map(|folder| {
                let children = folder
                    .children
                    .iter()
                    .map(|child| (child.path.clone(), child.count))
                    .collect();
                (folder.path.clone(), folder.count, children)
            })
            .collect()
    }

    fn amda() -> Vec<PublicCatalogue> {
        vec![
            entry(
                "bow shocks",
                Some("AMDA / EARTH"),
                Some("Cluster crossings"),
            ),
            entry("cusps", Some("AMDA/EARTH/Cusp"), None),
            entry("CMEs", Some("AMDA/Solar wind"), Some("Seen by ACE")),
            entry("jupiter", Some("amda-others"), None),
            entry("loose", None, Some("seen by ACE")),
            entry("blank", Some(" / "), None),
        ]
    }

    #[test]
    fn folders_nest_by_their_parts_and_count_what_they_hold_below() {
        assert_eq!(
            folders(&folder_tree(&amda())),
            vec![
                (
                    "AMDA".to_owned(),
                    3,
                    vec![
                        ("AMDA/EARTH".to_owned(), 2),
                        ("AMDA/Solar wind".to_owned(), 1)
                    ]
                ),
                ("amda-others".to_owned(), 1, vec![]),
            ]
        );
        let earth = &folder_tree(&amda())[0].children[0];
        assert_eq!(earth.name, "EARTH");
        assert_eq!(earth.children[0].path, "AMDA/EARTH/Cusp");
        assert_eq!(earth.children[0].count, 1);
        assert!(folder_tree(&[]).is_empty());
    }

    #[test]
    fn a_catalogue_is_in_its_folder_and_every_folder_above_it() {
        let cusps = &amda()[1];
        assert_eq!(cusps.folder_path().as_deref(), Some("AMDA/EARTH/Cusp"));
        assert!(cusps.in_folder("AMDA"));
        assert!(cusps.in_folder("AMDA/EARTH"));
        assert!(cusps.in_folder("AMDA/EARTH/Cusp"));
        assert!(
            !cusps.in_folder("AMDA/EAR"),
            "a prefix of a name is not a folder"
        );
        assert!(!amda()[3].in_folder("amda"));
        assert_eq!(amda()[4].folder_path(), None);
        assert_eq!(
            amda()[5].folder_path(),
            None,
            "a folder of blanks is no folder"
        );
    }

    #[test]
    fn the_folders_above_one_are_open() {
        assert_eq!(
            open_folders("AMDA / EARTH/Cusp"),
            vec!["AMDA", "AMDA/EARTH", "AMDA/EARTH/Cusp"]
        );
        assert!(open_folders("").is_empty());
        assert_eq!(folder_label("AMDA/EARTH"), "AMDA / EARTH");
    }

    #[test]
    fn a_search_looks_in_every_folder_for_all_and_only_in_the_folder_otherwise() {
        let all = amda();
        let found = |folder: Option<&str>, search: &str| {
            index(&all, folder, search)
                .iter()
                .flat_map(|shelf| shelf.catalogues.iter().map(|c| c.name.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(found(None, "ace"), vec!["CMEs", "loose"]);
        assert_eq!(found(Some("AMDA"), "ace"), vec!["CMEs"]);
        assert_eq!(found(Some("AMDA/EARTH"), "ace"), Vec::<String>::new());
        assert_eq!(found(Some("AMDA/EARTH"), ""), vec!["bow shocks", "cusps"]);
        assert_eq!(found(None, "").len(), 6);
    }

    #[test]
    fn folders_written_with_or_without_spaces_share_a_shelf() {
        let all = [
            entry("a", Some("AMDA / EARTH"), None),
            entry("b", Some("AMDA/EARTH"), None),
            entry("c", Some(" / "), None),
        ];
        assert_eq!(
            names(&shelves(&all, "")),
            vec![
                ("AMDA / EARTH".to_owned(), vec!["a", "b"]),
                ("Other".to_owned(), vec!["c"]),
            ]
        );
    }
}
