//! Links from a project to catalogues of other projects.

use cocat_core::model::{
    self, LinkError, NewCatalogue, NewLink, add_link, catalogue, catalogue_events,
    create_catalogue, links, remove_link,
};
use cocat_core::{tscat_json, votable};
use yrs::{Any, Doc, In, Map, MapPrelim, Transact, WriteTxn};

const HERE: &str = "project-here";

fn new_link(project: &str, catalogue: &str, name: &str) -> NewLink {
    NewLink {
        project: project.to_owned(),
        catalogue: catalogue.to_owned(),
        name: name.to_owned(),
        by: Some("a@x.org".to_owned()),
        at: 1_700_000_000_000,
    }
}

fn link(doc: &Doc, project: &str, catalogue: &str, name: &str) -> Result<String, LinkError> {
    add_link(
        &mut doc.transact_mut(),
        HERE,
        new_link(project, catalogue, name),
    )
}

#[test]
fn a_project_without_links_has_none() {
    assert!(links(&Doc::new().transact()).is_empty());
}

#[test]
fn a_link_is_kept_with_who_and_when() {
    let doc = Doc::new();

    let id = link(&doc, "other", "cat-1", "Shocks").unwrap();

    let found = links(&doc.transact());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, id);
    assert_eq!(found[0].project, "other");
    assert_eq!(found[0].catalogue, "cat-1");
    assert_eq!(found[0].name, "Shocks");
    assert_eq!(found[0].by.as_deref(), Some("a@x.org"));
    assert_eq!(found[0].at, 1_700_000_000_000);
}

#[test]
fn links_are_sorted_by_name_ignoring_case() {
    let doc = Doc::new();
    link(&doc, "o", "1", "banana").unwrap();
    link(&doc, "o", "2", "Apple").unwrap();
    link(&doc, "o", "3", "cherry").unwrap();

    let names: Vec<String> = links(&doc.transact()).into_iter().map(|l| l.name).collect();

    assert_eq!(names, ["Apple", "banana", "cherry"]);
}

#[test]
fn the_same_catalogue_is_linked_once() {
    let doc = Doc::new();
    link(&doc, "other", "cat-1", "Shocks").unwrap();

    let again = link(&doc, "other", "cat-1", "Shocks again");

    assert!(matches!(again, Err(LinkError::AlreadyLinked)));
    assert_eq!(links(&doc.transact()).len(), 1);
    assert!(link(&doc, "other", "cat-2", "Other one").is_ok());
    assert!(link(&doc, "third", "cat-1", "Same uuid, other project").is_ok());
}

#[test]
fn a_project_does_not_link_its_own_catalogue() {
    let doc = Doc::new();

    let own = link(&doc, HERE, "cat-1", "Mine");

    assert!(matches!(own, Err(LinkError::SameProject)));
    assert!(links(&doc.transact()).is_empty());
}

#[test]
fn removing_a_link_leaves_the_others() {
    let doc = Doc::new();
    let first = link(&doc, "o", "1", "one").unwrap();
    link(&doc, "o", "2", "two").unwrap();

    assert!(remove_link(&mut doc.transact_mut(), &first));
    assert!(!remove_link(&mut doc.transact_mut(), &first));

    let names: Vec<String> = links(&doc.transact()).into_iter().map(|l| l.name).collect();
    assert_eq!(names, ["two"]);
}

#[test]
fn a_malformed_link_is_skipped() {
    let doc = Doc::new();
    link(&doc, "o", "1", "good").unwrap();
    {
        let mut txn = doc.transact_mut();
        let root = txn.get_or_insert_map("links");
        root.insert(
            &mut txn,
            "no-project",
            MapPrelim::from_iter([("catalogue".to_owned(), In::Any(Any::from("c")))]),
        );
        root.insert(&mut txn, "not-a-map", "text");
    }

    let names: Vec<String> = links(&doc.transact()).into_iter().map(|l| l.name).collect();

    assert_eq!(names, ["good"]);
}

fn project_with_a_catalogue() -> Doc {
    let doc = Doc::new();
    let new = NewCatalogue {
        uuid: "cat-own".to_owned(),
        name: "Own".to_owned(),
        ..NewCatalogue::default()
    };
    create_catalogue(&mut doc.transact_mut(), &new).unwrap();
    doc
}

fn exports(doc: &Doc) -> (String, String) {
    let txn = doc.transact();
    let summary = catalogue(&txn, "cat-own").unwrap();
    let events = catalogue_events(&txn, "cat-own").unwrap().unwrap();
    let pair = [(&summary, events.as_slice())];
    (
        tscat_json::export(&txn, &pair).unwrap(),
        votable::export_many(&pair).unwrap(),
    )
}

#[test]
fn links_never_reach_an_export() {
    let doc = project_with_a_catalogue();
    let without = exports(&doc);

    link(&doc, "other", "cat-1", "Shocks").unwrap();

    assert_eq!(exports(&doc), without);
    assert!(model::catalogues(&doc.transact()).len() == 1);
}
