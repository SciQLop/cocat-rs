//! The file tree of a workspace, read from its manifest.

use serde_json::Value;
use std::collections::BTreeMap;

use crate::format::whole_number;

pub use cocat_core::files::FileKind;
use cocat_core::files::{TEXTS, extension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub kind: FileKind,
    pub doc: Option<String>,
    pub hash: Option<String>,
    pub size: Option<u64>,
    pub by: Option<String>,
    pub at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Folder {
        name: String,
        path: String,
        children: Vec<Node>,
    },
    File(Entry),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    Notebook,
    Text { language: Option<&'static str> },
    Image,
    Download,
}

/// No `svg`: a stored blob is served as bytes, and serving it as an image would let its
/// scripts run on the app's origin; a stored SVG is a download.
const IMAGES: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

pub fn entries(files: &Value) -> Vec<Entry> {
    let Some(map) = files.as_object() else {
        return Vec::new();
    };
    map.iter()
        .filter(|(path, _)| !path.is_empty())
        .filter_map(|(path, value)| entry(path, value.as_object()?))
        .collect()
}

fn entry(path: &str, fields: &serde_json::Map<String, Value>) -> Option<Entry> {
    let text = |key: &str| fields.get(key)?.as_str().map(str::to_owned);
    Some(Entry {
        path: path.to_owned(),
        kind: kind(fields.get("kind")),
        doc: text("doc").filter(|doc| uuid::Uuid::parse_str(doc).is_ok()),
        hash: text("hash"),
        size: fields
            .get("size")
            .and_then(whole_number)
            .and_then(|n| u64::try_from(n).ok()),
        by: text("by"),
        at: fields.get("at").and_then(whole_number),
    })
}

fn kind(value: Option<&Value>) -> FileKind {
    match value.and_then(Value::as_str) {
        Some("notebook") => FileKind::Notebook,
        Some("text") => FileKind::Text,
        _ => FileKind::File,
    }
}

pub fn tree(entries: Vec<Entry>) -> Vec<Node> {
    level(entries, 0, "")
}

fn level(entries: Vec<Entry>, depth: usize, prefix: &str) -> Vec<Node> {
    let mut folders: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
    let mut files = Vec::new();
    for entry in entries {
        let mut segments = entry.path.split('/').skip(depth);
        let first = segments.next().unwrap_or_default().to_owned();
        if segments.next().is_some() {
            folders.entry(first).or_default().push(entry);
        } else {
            files.push(entry);
        }
    }
    let mut nodes: Vec<Node> = folders
        .into_iter()
        .map(|(name, inside)| {
            let path = format!("{prefix}{name}");
            let children = level(inside, depth + 1, &format!("{path}/"));
            Node::Folder {
                name,
                path,
                children,
            }
        })
        .collect();
    nodes.sort_by_cached_key(|node| sort_key(node_name(node)));
    files.sort_by_cached_key(|entry| sort_key(file_name(&entry.path)));
    nodes.extend(files.into_iter().map(Node::File));
    nodes
}

fn node_name(node: &Node) -> &str {
    match node {
        Node::Folder { name, .. } => name,
        Node::File(entry) => file_name(&entry.path),
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn sort_key(name: &str) -> (String, String) {
    (name.to_lowercase(), name.to_owned())
}

pub fn shows_as(entry: &Entry) -> Shown {
    match entry.kind {
        FileKind::Notebook => Shown::Notebook,
        FileKind::Text => Shown::Text {
            language: language_of(&entry.path),
        },
        FileKind::File => by_extension(&entry.path),
    }
}

fn by_extension(path: &str) -> Shown {
    let ext = extension(path);
    if IMAGES.contains(&ext.as_str()) {
        return Shown::Image;
    }
    match TEXTS.iter().find(|(known, _)| *known == ext) {
        Some((_, language)) => Shown::Text {
            language: *language,
        },
        None => Shown::Download,
    }
}

pub fn is_image(path: &str) -> bool {
    by_extension(path) == Shown::Image
}

/// The stored hash of the image a markdown link `dest` in the file at `from` points at.
pub fn placed_image<'a>(from: &str, dest: &str, files: &'a [Entry]) -> Option<&'a str> {
    let path = resolve_path(from, dest)?;
    if !is_image(&path) {
        return None;
    }
    files
        .iter()
        .find(|entry| entry.path == path)?
        .hash
        .as_deref()
}

/// Whether the opened file still shows the same thing: who changed it, when and its size
/// only change the line above it.
pub fn same_view(a: Option<&Entry>, b: Option<&Entry>) -> bool {
    let shown = |entry: &Entry| {
        (
            entry.path.clone(),
            entry.kind,
            entry.doc.clone(),
            entry.hash.clone(),
        )
    };
    a.map(shown) == b.map(shown)
}

fn language_of(path: &str) -> Option<&'static str> {
    let ext = extension(path);
    TEXTS
        .iter()
        .find(|(known, _)| *known == ext)
        .and_then(|(_, language)| *language)
}

/// The workspace path a link `relative` in the file at `from` points at, `None` when it
/// climbs above the workspace. The link's `%XX` escapes are decoded; a `#` or `?` part is
/// dropped.
pub fn resolve_path(from: &str, relative: &str) -> Option<String> {
    let relative = relative.split(['#', '?']).next().unwrap_or_default();
    let decoded = percent_encoding::percent_decode_str(relative)
        .decode_utf8()
        .ok()?;
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    for part in decoded.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            name => parts.push(name),
        }
    }
    Some(parts.join("/"))
}

/// A size as people read it: "512 B", "4.2 kB", "2.3 MB".
pub fn size_text(bytes: u64) -> String {
    let bytes_f = bytes as f64;
    match bytes {
        0..1_000 => format!("{bytes} B"),
        1_000..1_000_000 => format!("{:.1} kB", bytes_f / 1e3),
        _ => format!("{:.1} MB", bytes_f / 1e6),
    }
}

/// The versions, newest first, where a whole file's stored content (`None`: not there)
/// differs from the version before: the ones its history lists. A file deleted then
/// brought back shows both changes; one that never existed shows none.
pub fn versions_that_changed(newest_first: &[(u64, Option<String>)]) -> Vec<u64> {
    newest_first
        .iter()
        .enumerate()
        .filter(|(index, (_, content))| match newest_first.get(index + 1) {
            Some((_, older)) => content != older,
            None => content.is_some(),
        })
        .map(|(_, (version, _))| *version)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DOC: &str = "0b6f3e7c-2a41-4d8e-9c55-7f1a2b3c4d5e";

    fn file(path: &str) -> Entry {
        Entry {
            path: path.into(),
            kind: FileKind::File,
            doc: None,
            hash: None,
            size: None,
            by: None,
            at: None,
        }
    }

    fn names(nodes: &[Node]) -> Vec<String> {
        nodes
            .iter()
            .map(|node| match node {
                Node::Folder { path, .. } => format!("{path}/"),
                Node::File(entry) => entry.path.clone(),
            })
            .collect()
    }

    #[test]
    fn relative_paths_resolve_from_the_notebooks_folder() {
        let from = "notebooks/analysis.ipynb";
        assert_eq!(
            resolve_path(from, "../figures/fig1.png").as_deref(),
            Some("figures/fig1.png")
        );
        assert_eq!(
            resolve_path(from, "./plots/a%20b.png").as_deref(),
            Some("notebooks/plots/a b.png")
        );
        assert_eq!(
            resolve_path("a.ipynb", "../x.png"),
            None,
            "nothing above the workspace"
        );
        assert_eq!(
            resolve_path("a.ipynb", "x.png#top").as_deref(),
            Some("x.png")
        );
    }

    #[test]
    fn sizes_read_in_their_unit() {
        assert_eq!(size_text(512), "512 B");
        assert_eq!(size_text(4200), "4.2 kB");
        assert_eq!(size_text(2_345_678), "2.3 MB");
    }

    #[test]
    fn a_whole_file_lists_only_the_versions_that_changed_it() {
        let newest_first = [
            (5, Some("c".to_owned())),
            (4, Some("b".to_owned())),
            (3, Some("b".to_owned())),
            (2, None),
            (1, Some("a".to_owned())),
        ];
        assert_eq!(versions_that_changed(&newest_first), [5, 3, 2, 1]);
        assert_eq!(
            versions_that_changed(&[(1, None)]),
            Vec::<u64>::new(),
            "never there"
        );
    }

    #[test]
    fn entries_skip_what_is_not_a_file() {
        let files = json!({
            "a.py": {"kind": "text", "doc": DOC, "size": 12.0, "by": "me", "at": 5},
            "bad": "nope",
            "": {"kind": "text"},
            "n.ipynb": {"kind": "notebook", "hash": "abc"},
        });
        let found = entries(&files);
        assert_eq!(found.len(), 2);
        let a = found.iter().find(|e| e.path == "a.py").unwrap();
        assert_eq!(a.kind, FileKind::Text);
        assert_eq!(a.doc.as_deref(), Some(DOC));
        assert_eq!(a.size, Some(12));
        assert_eq!(a.by.as_deref(), Some("me"));
        assert_eq!(a.at, Some(5));
        let n = found.iter().find(|e| e.path == "n.ipynb").unwrap();
        assert_eq!(n.hash.as_deref(), Some("abc"));
        assert!(entries(&json!([1])).is_empty());
    }

    #[test]
    fn an_unknown_kind_is_a_plain_file() {
        let found = entries(&json!({"x": {"kind": "weird"}, "y": {}}));
        assert!(found.iter().all(|e| e.kind == FileKind::File));
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn the_tree_lists_folders_first_by_name() {
        let nodes = tree(vec![
            file("b.txt"),
            file("A/x.py"),
            file("a/y.py"),
            file("c.ipynb"),
            file("A/sub/z.py"),
        ]);
        assert_eq!(names(&nodes), ["A/", "a/", "b.txt", "c.ipynb"]);
        let Node::Folder { children, .. } = &nodes[0] else {
            panic!("folder expected");
        };
        assert_eq!(names(children), ["A/sub/", "A/x.py"]);
    }

    #[test]
    fn each_extension_shows_as_expected() {
        let shown = |path: &str, kind| shows_as(&Entry { kind, ..file(path) });
        let file_kind = FileKind::File;
        assert_eq!(shown("p.PNG", file_kind), Shown::Image);
        assert_eq!(
            shown("a.py", file_kind),
            Shown::Text {
                language: Some("py")
            }
        );
        assert_eq!(
            shown("a.yml", file_kind),
            Shown::Text {
                language: Some("yaml")
            }
        );
        assert_eq!(shown("a.txt", file_kind), Shown::Text { language: None });
        assert_eq!(shown("a.bin", file_kind), Shown::Download);
        assert_eq!(shown("Makefile", file_kind), Shown::Download);
        assert_eq!(shown("n.ipynb", FileKind::Notebook), Shown::Notebook);
        assert_eq!(
            shown("notes.md", FileKind::Text),
            Shown::Text {
                language: Some("md")
            }
        );
        assert_eq!(
            shown("LICENSE", FileKind::Text),
            Shown::Text { language: None }
        );
    }

    #[test]
    fn a_doc_that_is_not_a_uuid_is_dropped() {
        let found = entries(&json!({
            "a.py": {"kind": "text", "doc": "../OTHER/DOC"},
            "b.py": {"kind": "text", "doc": DOC},
        }));
        let doc = |path: &str| found.iter().find(|e| e.path == path).unwrap().doc.clone();
        assert_eq!(doc("a.py"), None, "the entry stays, not shared yet");
        assert_eq!(doc("b.py").as_deref(), Some(DOC));
    }

    #[test]
    fn a_stored_svg_is_a_download() {
        assert_eq!(shows_as(&file("d/i.svg")), Shown::Download);
        assert!(!is_image("d/i.SVG"));
        assert!(is_image("d/i.webp"));
    }

    #[test]
    fn a_markdown_image_places_only_stored_raster_images() {
        let stored = |path: &str| Entry {
            hash: Some(format!("hash-of-{path}")),
            ..file(path)
        };
        let files = [stored("fig/a.png"), stored("fig/b.svg"), file("fig/c.png")];
        let from = "nb/n.ipynb";
        assert_eq!(
            placed_image(from, "../fig/a.png", &files),
            Some("hash-of-fig/a.png")
        );
        assert_eq!(
            placed_image(from, "../fig/b.svg", &files),
            None,
            "svg is never placed"
        );
        assert_eq!(
            placed_image(from, "../fig/c.png", &files),
            None,
            "not stored"
        );
        assert_eq!(
            placed_image(from, "../fig/d.png", &files),
            None,
            "not there"
        );
    }

    #[test]
    fn a_new_size_or_author_does_not_reopen_the_file() {
        let shown = Entry {
            doc: Some(DOC.into()),
            ..file("a.py")
        };
        let touched = Entry {
            size: Some(9),
            by: Some("bob".into()),
            at: Some(7),
            ..shown.clone()
        };
        assert!(same_view(Some(&shown), Some(&touched)));
        assert!(same_view(None, None));
        let rehashed = Entry {
            hash: Some("h".into()),
            ..shown.clone()
        };
        assert!(!same_view(Some(&shown), Some(&rehashed)));
        assert!(!same_view(Some(&shown), None));
        assert!(!same_view(Some(&shown), Some(&file("b.py"))));
    }
}
