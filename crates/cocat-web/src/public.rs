//! The public catalogues' index: a folder tree, and shelves by folder narrowed by a search.

use cocat_core::role::Role;
use cocat_core::text_search::{Catalogue, rank_catalogues};
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
    /// The provider it follows, when the server keeps it in step with one.
    #[serde(default)]
    pub relay: Option<Relay>,
}

/// Where a relayed catalogue comes from, as `GET /api/public` says.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Relay {
    pub source: String,
    pub url: Option<String>,
    pub refreshed_at: Option<String>,
    pub published: bool,
}

impl Relay {
    /// "From DONKI (NASA CCMC), refreshed 2026-11-11", or why it no longer changes.
    pub fn text(&self) -> String {
        if !self.published {
            return format!("No longer published by {}", self.source);
        }
        match self.refreshed_at.as_deref().and_then(|at| at.get(..10)) {
            Some(day) => format!("From {}, refreshed {day}", self.source),
            None => format!("From {}", self.source),
        }
    }
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
            files: false,
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

    /// What the search reads of it.
    pub fn searched(&self) -> Catalogue<'_> {
        Catalogue {
            name: &self.name,
            folder: self.folder.as_deref(),
            source: self.source.as_deref(),
            description: self.description.as_deref(),
        }
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

/// The catalogues that answer `search`, shelved by folder. Without a search: every one, shelves
/// by folder (unfiled last), each by name. With one: those ranked by BM25 (see
/// [`cocat_core::text_search`]) best first, a shelf where its best catalogue ranks; then those
/// holding a word only inside a longer one (`reconn`), while it is being typed.
pub fn shelves(all: &[PublicCatalogue], search: &str) -> Vec<Shelf> {
    let kept = if search.trim().is_empty() {
        by_folder(all)
    } else {
        ranked(all, search)
    };
    let mut shelves: Vec<Shelf> = Vec::new();
    for catalogue in kept {
        let folder = catalogue.folder_path();
        match shelves.iter_mut().find(|shelf| shelf.folder == folder) {
            Some(shelf) => shelf.catalogues.push(catalogue.clone()),
            None => shelves.push(Shelf {
                folder,
                catalogues: vec![catalogue.clone()],
            }),
        }
    }
    shelves
}

fn by_folder(all: &[PublicCatalogue]) -> Vec<&PublicCatalogue> {
    let mut kept: Vec<&PublicCatalogue> = all.iter().collect();
    kept.sort_by_key(|c| {
        let folder = c.folder_path();
        (
            folder.is_none(),
            folder.map(|path| path.to_lowercase()),
            c.name.to_lowercase(),
        )
    });
    kept
}

fn ranked<'a>(all: &'a [PublicCatalogue], search: &str) -> Vec<&'a PublicCatalogue> {
    let searched: Vec<Catalogue> = all.iter().map(PublicCatalogue::searched).collect();
    rank_catalogues(&searched, search)
        .into_iter()
        .map(|at| &all[at])
        .collect()
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
            relay: None,
        }
    }

    fn relay(published: bool, refreshed: Option<&str>) -> Relay {
        Relay {
            source: "DONKI (NASA CCMC)".into(),
            url: Some("https://api.nasa.gov/DONKI/FLR".into()),
            refreshed_at: refreshed.map(str::to_owned),
            published,
        }
    }

    #[test]
    fn a_relayed_catalogue_says_where_it_comes_from_and_how_fresh_it_is() {
        let fresh = relay(true, Some("2026-11-11T04:00:12Z"));
        assert_eq!(fresh.text(), "From DONKI (NASA CCMC), refreshed 2026-11-11");
        assert_eq!(relay(true, None).text(), "From DONKI (NASA CCMC)");
        assert_eq!(
            relay(false, Some("2026-11-11T04:00:12Z")).text(),
            "No longer published by DONKI (NASA CCMC)"
        );
    }

    #[test]
    fn the_index_reads_where_a_relayed_catalogue_comes_from() {
        let body = r#"{"id":"public.donki.flr","name":"Solar flares","folder":"DONKI/Solar flares",
            "source":"DONKI (NASA CCMC)","description":null,"role":"viewer","changed":null,"count":3,
            "relay":{"source":"DONKI (NASA CCMC)","url":null,"refreshed_at":"2026-11-11T04:00:12Z","published":true}}"#;
        let read: PublicCatalogue = serde_json::from_str(body).unwrap();
        assert_eq!(
            read.relay,
            Some(relay(true, Some("2026-11-11T04:00:12Z"))).map(|r| Relay { url: None, ..r })
        );
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
    fn a_search_finds_a_word_in_any_field() {
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
        assert!(shelves(&all, "magnetopause").is_empty());
    }

    #[test]
    fn a_search_ranks_the_best_answer_first_and_its_shelf_with_it() {
        let all = [
            entry(
                "Wind ICMEs",
                Some("Literature/Heliosphere"),
                Some("Compared with Solar Orbiter."),
            ),
            entry("Mirror mode storms", Some("Literature/Heliosphere"), None),
            entry(
                "Solar Orbiter reconnection exhausts",
                Some("Literature/Heliosphere"),
                None,
            ),
            entry("Reconnection at Saturn", Some("Literature/Saturn"), None),
        ];
        assert_eq!(
            names(&shelves(&all, "solar orbiter reconnection")),
            vec![
                (
                    "Literature / Heliosphere".to_owned(),
                    vec!["Solar Orbiter reconnection exhausts", "Wind ICMEs"],
                ),
                (
                    "Literature / Saturn".to_owned(),
                    vec!["Reconnection at Saturn"]
                ),
            ]
        );
    }

    #[test]
    fn a_word_still_being_typed_finds_what_it_starts() {
        let all = [
            entry("Reconnection exhausts", None, None),
            entry("Shocks", None, None),
        ];
        assert_eq!(
            names(&shelves(&all, "reconn")),
            vec![("Other".to_owned(), vec!["Reconnection exhausts"])]
        );
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
            let mut names: Vec<_> = index(&all, folder, search)
                .iter()
                .flat_map(|shelf| shelf.catalogues.iter().map(|c| c.name.clone()))
                .collect();
            names.sort();
            names
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
