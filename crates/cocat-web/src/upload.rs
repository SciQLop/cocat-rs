//! Uploading a SciQLop workspace's files: where each one lands, which document it reuses,
//! and what the page says about it.

use cocat_core::files::{FileKind, Skip};
use sha2::{Digest, Sha256};

use crate::workspace::Entry;

/// Why a picked file did not reach the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    Skipped(Skip),
    /// The bytes are not what the name says (a notebook that is not JSON, text that is not UTF-8).
    Unreadable(String),
    /// The server said no, in its words.
    Refused(String),
}

/// The path a picked file takes in the project: inside its folder for a folder upload.
pub fn path_of(name: &str, relative: &str) -> String {
    let path = if relative.is_empty() { name } else { relative };
    path.trim_start_matches('/').to_owned()
}

/// The document an upload to `path` writes into: the one already there when it holds the
/// same kind, so its history keeps the version it replaces.
pub fn kept_document(files: &[Entry], path: &str, kind: FileKind) -> Option<String> {
    files
        .iter()
        .find(|entry| entry.path == path && entry.kind == kind)
        .and_then(|entry| entry.doc.clone())
}

/// What a whole file is stored under.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn uploaded_text(count: usize) -> String {
    match count {
        1 => "Uploaded 1 file.".to_owned(),
        count => format!("Uploaded {count} files."),
    }
}

pub fn failure_text(path: &str, failure: &Failure) -> String {
    let why = match failure {
        Failure::Skipped(Skip::Rules) => "left out by the sharing rules".to_owned(),
        Failure::Skipped(Skip::TooBig { cap }) => {
            format!("too big to share (over {} MB)", cap / (1024 * 1024))
        }
        Failure::Unreadable(why) | Failure::Refused(why) => why.clone(),
    };
    format!("Skipped: {path} — {why}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, kind: FileKind, doc: Option<&str>) -> Entry {
        Entry {
            path: path.into(),
            kind,
            doc: doc.map(str::to_owned),
            hash: None,
            size: None,
            by: None,
            at: None,
        }
    }

    #[test]
    fn a_file_of_a_folder_keeps_its_place_in_it() {
        assert_eq!(path_of("a.py", ""), "a.py");
        assert_eq!(path_of("a.py", "study/code/a.py"), "study/code/a.py");
        assert_eq!(path_of("a.py", "/study/a.py"), "study/a.py");
    }

    #[test]
    fn an_upload_writes_into_the_document_the_path_already_has() {
        let files = [
            entry("a.ipynb", FileKind::Notebook, Some("d1")),
            entry("b.py", FileKind::Text, Some("d2")),
        ];
        assert_eq!(
            kept_document(&files, "a.ipynb", FileKind::Notebook).as_deref(),
            Some("d1")
        );
        assert_eq!(kept_document(&files, "b.py", FileKind::Notebook), None);
        assert_eq!(kept_document(&files, "c.py", FileKind::Text), None);
    }

    #[test]
    fn a_whole_file_is_named_by_its_sha256() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn the_page_says_what_was_uploaded_and_what_was_not() {
        assert_eq!(uploaded_text(1), "Uploaded 1 file.");
        assert_eq!(uploaded_text(3), "Uploaded 3 files.");
        assert_eq!(
            failure_text("x.cdf", &Failure::Skipped(Skip::Rules)),
            "Skipped: x.cdf — left out by the sharing rules"
        );
        let cap = 20 * 1024 * 1024;
        assert_eq!(
            failure_text("big.png", &Failure::Skipped(Skip::TooBig { cap })),
            "Skipped: big.png — too big to share (over 20 MB)"
        );
        assert_eq!(
            failure_text("n.ipynb", &Failure::Unreadable("not a notebook".into())),
            "Skipped: n.ipynb — not a notebook"
        );
    }
}
