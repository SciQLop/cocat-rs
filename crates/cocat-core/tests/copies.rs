//! The chain of copies a catalogue descends from.

use cocat_core::model::{
    self, Hop, NewCatalogue, adopt, catalogue, copy_chain, create_catalogue, extract,
    set_copied_from,
};
use yrs::{Any, Array, ArrayPrelim, Doc, In, Map, MapPrelim, Transact, WriteTxn};

const SOURCE: &str = "6f9c1a52-0b0d-4f4e-9d5e-1b2c3d4e5f60";
const COPY: &str = "0e4d2c31-5a6b-4c7d-8e9f-0a1b2c3d4e5f";

fn hop(n: u64) -> Hop {
    Hop {
        project: format!("project-{n}"),
        project_name: format!("Project {n}"),
        catalogue: format!("catalogue-{n}"),
        catalogue_name: format!("Catalogue {n}"),
        version: Some(n),
        at: 1_700_000_000_000 + n as i64,
        by: Some("a@x.org".to_owned()),
    }
}

fn doc_with(uuids: &[&str]) -> Doc {
    let doc = Doc::new();
    {
        let mut txn = doc.transact_mut();
        for uuid in uuids {
            let new = NewCatalogue {
                uuid: (*uuid).to_owned(),
                name: (*uuid).to_owned(),
                ..NewCatalogue::default()
            };
            create_catalogue(&mut txn, &new).unwrap();
        }
    }
    doc
}

fn chain(doc: &Doc, uuid: &str) -> Vec<Hop> {
    catalogue(&doc.transact(), uuid).unwrap().copied_from
}

#[test]
fn a_catalogue_without_a_chain_has_none() {
    let doc = doc_with(&[SOURCE]);

    assert!(chain(&doc, SOURCE).is_empty());
    assert_eq!(copy_chain(&doc.transact(), SOURCE, hop(1)), [hop(1)]);
}

#[test]
fn a_copy_extends_its_sources_chain() {
    let doc = doc_with(&[SOURCE]);
    let mut txn = doc.transact_mut();
    set_copied_from(&mut txn, SOURCE, &[hop(1)]).unwrap();

    let extended = copy_chain(&txn, SOURCE, hop(2));
    assert_eq!(extended, [hop(1), hop(2)]);
    drop(txn);

    let mut txn = doc.transact_mut();
    set_copied_from(&mut txn, SOURCE, &extended).unwrap();
    drop(txn);
    assert_eq!(chain(&doc, SOURCE), [hop(1), hop(2)]);
}

#[test]
fn the_chain_keeps_the_original_and_the_latest() {
    let doc = doc_with(&[SOURCE]);
    let long: Vec<Hop> = (1..=32).map(hop).collect();
    set_copied_from(&mut doc.transact_mut(), SOURCE, &long).unwrap();

    let extended = copy_chain(&doc.transact(), SOURCE, hop(33));

    assert_eq!(extended.len(), 32);
    assert_eq!(extended[0], hop(1));
    assert_eq!(extended[1], hop(3));
    assert_eq!(extended[31], hop(33));
}

#[test]
fn a_malformed_chain_reads_what_parses() {
    let doc = doc_with(&[SOURCE]);
    {
        let mut txn = doc.transact_mut();
        let object = txn
            .get_or_insert_map(model::CATALOGUES)
            .get(&txn, SOURCE)
            .and_then(|out| out.cast::<yrs::MapRef>().ok())
            .unwrap();
        let good = serde_json::to_value(hop(1)).unwrap();
        let good: Any = serde_json::from_value(good).unwrap();
        let Any::Map(good) = good else { panic!() };
        let good = MapPrelim::from_iter(good.iter().map(|(k, v)| (k.clone(), In::Any(v.clone()))));
        let broken = MapPrelim::from_iter([("project".to_owned(), In::from("only"))]);
        let list = object.insert(&mut txn, "copied_from", ArrayPrelim::default());
        list.push_back(&mut txn, In::from("not a map"));
        list.push_back(&mut txn, broken);
        list.push_back(&mut txn, good);
    }

    assert_eq!(chain(&doc, SOURCE), [hop(1)]);
}

#[test]
fn adopting_a_proposal_keeps_the_targets_chain() {
    let public = doc_with(&[SOURCE]);
    set_copied_from(&mut public.transact_mut(), SOURCE, &[hop(1)]).unwrap();
    let proposed = extract(&public, SOURCE);
    set_copied_from(&mut proposed.transact_mut(), SOURCE, &[hop(7), hop(8)]).unwrap();

    adopt(&mut public.transact_mut(), &proposed, SOURCE);

    assert_eq!(chain(&public, SOURCE), [hop(1)]);
}

#[test]
fn adopting_does_not_give_the_target_a_chain_it_lacked() {
    let public = doc_with(&[SOURCE, COPY]);
    let proposed = extract(&public, COPY);
    set_copied_from(&mut proposed.transact_mut(), COPY, &[hop(7)]).unwrap();

    adopt(&mut public.transact_mut(), &proposed, COPY);

    assert!(chain(&public, COPY).is_empty());
}

#[test]
fn adopting_a_proposal_without_a_chain_keeps_the_targets_chain() {
    let public = doc_with(&[SOURCE]);
    set_copied_from(&mut public.transact_mut(), SOURCE, &[hop(1)]).unwrap();
    let proposed = doc_with(&[SOURCE]);

    adopt(&mut public.transact_mut(), &proposed, SOURCE);

    assert_eq!(chain(&public, SOURCE), [hop(1)]);
}

#[test]
fn an_extract_carries_the_chain() {
    let public = doc_with(&[SOURCE]);
    set_copied_from(&mut public.transact_mut(), SOURCE, &[hop(1)]).unwrap();

    assert_eq!(chain(&extract(&public, SOURCE), SOURCE), [hop(1)]);
}
