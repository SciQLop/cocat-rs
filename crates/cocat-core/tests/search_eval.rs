//! How well the public catalogues search answers real questions: a snapshot of the public
//! catalogues (`fixtures/search/public_catalogues.jsonl`, from cocat-dev on 2026-11-13) and
//! questions with the catalogues a person asking them wants (`queries.json`, judged by hand).
//!
//! Run with `cargo test -p cocat-core --test search_eval -- --nocapture` to see each query's
//! score. The test fails when the averages drop below the floors: a change to the search must
//! not answer worse than what it replaces. Raise the floors when it answers better.

use std::collections::HashSet;

use cocat_core::text_search::{Catalogue, rank_catalogues};
use serde::Deserialize;

/// The averages the search reached when the floors were last set: 0.903 and 1.000 with BM25F,
/// acronyms and DOIs (2026-11-13). BM25F alone scored 0.781 and 0.963, and the "every word"
/// filter before it 0.669 and 0.752.
const RECALL_FLOOR: f64 = 0.90;
const MRR_FLOOR: f64 = 0.99;

#[derive(Deserialize)]
struct Row {
    id: String,
    name: String,
    folder: Option<String>,
    source: Option<String>,
    description: Option<String>,
}

#[derive(Deserialize)]
struct Query {
    query: String,
    why: String,
    relevant: Vec<String>,
}

/// How one query did: the share of its wanted catalogues among the first `k` answers (`k` the
/// number wanted, at most 10), and one over the rank of the first wanted answer.
struct Score {
    recall: f64,
    reciprocal_rank: f64,
}

fn score(ranked: &[&str], relevant: &HashSet<&str>) -> Score {
    let k = relevant.len().min(10);
    let found = ranked
        .iter()
        .take(k)
        .filter(|id| relevant.contains(*id))
        .count();
    let first = ranked.iter().position(|id| relevant.contains(*id));
    Score {
        recall: found as f64 / k as f64,
        reciprocal_rank: first.map_or(0.0, |at| 1.0 / (at + 1) as f64),
    }
}

#[test]
fn the_search_answers_the_questions_at_least_as_well_as_before() {
    let rows: Vec<Row> = include_str!("fixtures/search/public_catalogues.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let queries: Vec<Query> =
        serde_json::from_str(include_str!("fixtures/search/queries.json")).unwrap();
    let catalogues: Vec<Catalogue> = rows
        .iter()
        .map(|row| Catalogue {
            name: &row.name,
            folder: row.folder.as_deref(),
            source: row.source.as_deref(),
            description: row.description.as_deref(),
        })
        .collect();

    let mut scores = Vec::new();
    println!("{:>6} {:>6}  query", "recall", "rr");
    for query in &queries {
        let relevant: HashSet<&str> = query.relevant.iter().map(String::as_str).collect();
        assert!(
            relevant
                .iter()
                .all(|id| rows.iter().any(|row| row.id == *id)),
            "{:?} names a catalogue the snapshot lacks",
            query.query
        );
        let ranked: Vec<&str> = rank_catalogues(&catalogues, &query.query)
            .into_iter()
            .map(|at| rows[at].id.as_str())
            .collect();
        let score = score(&ranked, &relevant);
        println!(
            "{:>6.2} {:>6.2}  {:?} ({})",
            score.recall, score.reciprocal_rank, query.query, query.why
        );
        scores.push(score);
    }
    let mean = |pick: fn(&Score) -> f64| scores.iter().map(pick).sum::<f64>() / scores.len() as f64;
    let (recall, mrr) = (mean(|s| s.recall), mean(|s| s.reciprocal_rank));
    println!(
        "mean recall {recall:.3}, MRR {mrr:.3} over {} queries",
        scores.len()
    );
    assert!(
        recall >= RECALL_FLOOR,
        "recall {recall:.3} below {RECALL_FLOOR}"
    );
    assert!(mrr >= MRR_FLOOR, "MRR {mrr:.3} below {MRR_FLOOR}");
}
