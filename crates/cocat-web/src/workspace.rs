//! The file tree of a workspace, read from its manifest.

use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Notebook,
    Text,
    File,
}

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

const IMAGES: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "svg"];

/// Extension to syntect lookup token; `None` means plain text.
const TEXTS: &[(&str, Option<&str>)] = &[
    ("py", Some("py")),
    ("md", Some("md")),
    ("txt", None),
    ("toml", Some("toml")),
    ("json", Some("json")),
    ("yaml", Some("yaml")),
    ("yml", Some("yaml")),
    ("cfg", Some("ini")),
    ("ini", Some("ini")),
    ("csv", None),
    ("sh", Some("sh")),
    ("rs", Some("rs")),
    ("c", Some("c")),
    ("cpp", Some("cpp")),
    ("h", Some("c")),
    ("js", Some("js")),
    ("ts", Some("ts")),
    ("html", Some("html")),
    ("css", Some("css")),
    ("tex", Some("tex")),
];

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
        doc: text("doc"),
        hash: text("hash"),
        size: fields
            .get("size")
            .and_then(whole)
            .and_then(|n| u64::try_from(n).ok()),
        by: text("by"),
        at: fields.get("at").and_then(whole),
    })
}

fn kind(value: Option<&Value>) -> FileKind {
    match value.and_then(Value::as_str) {
        Some("notebook") => FileKind::Notebook,
        Some("text") => FileKind::Text,
        _ => FileKind::File,
    }
}

/// Integers may arrive as floats from the browser side; accept the integer-valued ones.
fn whole(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        let f = value.as_f64()?;
        (f.fract() == 0.0 && f.abs() < 9.0e15).then_some(f as i64)
    })
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

fn language_of(path: &str) -> Option<&'static str> {
    let ext = extension(path);
    TEXTS
        .iter()
        .find(|(known, _)| *known == ext)
        .and_then(|(_, language)| *language)
}

fn extension(path: &str) -> String {
    file_name(path)
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_lowercase())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn entries_skip_what_is_not_a_file() {
        let files = json!({
            "a.py": {"kind": "text", "doc": "d1", "size": 12.0, "by": "me", "at": 5},
            "bad": "nope",
            "": {"kind": "text"},
            "n.ipynb": {"kind": "notebook", "hash": "abc"},
        });
        let found = entries(&files);
        assert_eq!(found.len(), 2);
        let a = found.iter().find(|e| e.path == "a.py").unwrap();
        assert_eq!(a.kind, FileKind::Text);
        assert_eq!(a.doc.as_deref(), Some("d1"));
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
        assert_eq!(shown("d/i.svg", file_kind), Shown::Image);
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
}
