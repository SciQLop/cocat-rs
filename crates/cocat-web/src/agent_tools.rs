//! What the page offers an AI agent running in the browser, through WebMCP
//! (<https://webmachinelearning.github.io/webmcp/>): tools it calls instead of clicking. Their
//! definitions and answers are plain data here; `views/webmcp.rs` registers them.
//!
//! The tools only do what the visitor can already do on the page, with the visitor's access.

use cocat_core::text_search::rank_catalogues;
use serde_json::{Value, json};

use crate::public::PublicCatalogue;

pub const SEARCH: &str = "search_public_catalogues";
pub const OPEN: &str = "open_public_catalogue";

/// How many catalogues a search answers with unless asked for another number, and at most.
const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 50;

/// The tools, as WebMCP's `registerTool` takes them, without their `execute`.
pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": SEARCH,
            "title": "Search the public catalogues",
            "description": "Search cocat's public catalogues of space physics events (lists from \
                AMDA, NASA DONKI, CDAW, HELIO4CAST, published papers, ...) by mission, topic, \
                author, paper title or DOI, best answer first. Use it to find catalogues about \
                a subject, or to check whether a paper's event list is already in cocat by \
                passing its DOI or link.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Words, a DOI or a link to a paper, for instance \
                            \"Solar Orbiter reconnection\" or \"10.1051/0004-6361/202346043\"."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_LIMIT,
                        "description": "How many catalogues to return (10 by default)."
                    }
                },
                "required": ["query"]
            },
            // Descriptions are written by each catalogue's authors, not by cocat.
            "annotations": {"readOnlyHint": true, "untrustedContentHint": true}
        }),
        json!({
            "name": OPEN,
            "title": "Open a public catalogue",
            "description": "Open one public catalogue in this page, by the id a search returned, \
                to show its events to the user.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "The catalogue's id, public.…"}
                },
                "required": ["id"]
            },
            "annotations": {"readOnlyHint": true}
        }),
    ]
}

/// The search's answer: the best catalogues for `input`'s query, each with what tells it apart.
pub fn search(all: &[PublicCatalogue], input: &Value) -> Result<Value, String> {
    let query = input
        .get("query")
        .and_then(Value::as_str)
        .filter(|query| !query.trim().is_empty())
        .ok_or("give a query")?;
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_LIMIT, |limit| (limit as usize).clamp(1, MAX_LIMIT));
    let searched: Vec<_> = all.iter().map(PublicCatalogue::searched).collect();
    let found: Vec<Value> = rank_catalogues(&searched, query)
        .into_iter()
        .take(limit)
        .map(|at| described(&all[at]))
        .collect();
    Ok(json!({"query": query, "catalogues": found}))
}

fn described(catalogue: &PublicCatalogue) -> Value {
    json!({
        "id": catalogue.id,
        "name": catalogue.name,
        "folder": catalogue.folder,
        "source": catalogue.source,
        "description": catalogue.description,
        "events": catalogue.count,
        "changed": catalogue.day(),
    })
}

/// The catalogue `input` names by id.
pub fn chosen<'a>(
    all: &'a [PublicCatalogue],
    input: &Value,
) -> Result<&'a PublicCatalogue, String> {
    let id = input
        .get("id")
        .and_then(Value::as_str)
        .ok_or("give an id")?;
    all.iter()
        .find(|catalogue| catalogue.id == id)
        .ok_or_else(|| format!("no public catalogue {id}; search for one first"))
}

#[cfg(test)]
mod tests {
    use cocat_core::role::Role;
    use serde_json::json;

    use super::*;

    fn entry(id: &str, name: &str, description: &str) -> PublicCatalogue {
        PublicCatalogue {
            id: id.into(),
            name: name.into(),
            folder: Some("Literature/Heliosphere".into()),
            source: Some("Published lists".into()),
            description: Some(description.into()),
            role: Role::Viewer,
            changed: Some("2026-11-13T10:00:00Z".into()),
            count: Some(146),
            relay: None,
        }
    }

    fn all() -> Vec<PublicCatalogue> {
        vec![
            entry(
                "public.a",
                "Mirror mode storms at Solar Orbiter",
                "Cite: doi.org/10.1029/x",
            ),
            entry(
                "public.b",
                "Reconnection exhausts, Solar Orbiter",
                "Cite: https://doi.org/10.1051/0004-6361/202346043",
            ),
            entry("public.c", "Saturn reconnection", ""),
        ]
    }

    fn ids(answer: &Value) -> Vec<&str> {
        answer["catalogues"]
            .as_array()
            .unwrap()
            .iter()
            .map(|catalogue| catalogue["id"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn the_tools_say_what_they_take_and_that_they_change_nothing() {
        let tools = definitions();
        assert_eq!(tools.len(), 2);
        for tool in &tools {
            assert!(tool["description"].as_str().unwrap().len() > 40);
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert_eq!(tool["annotations"]["readOnlyHint"], true);
        }
        assert_eq!(tools[0]["name"], SEARCH);
        assert_eq!(tools[0]["annotations"]["untrustedContentHint"], true);
        assert_eq!(tools[1]["name"], OPEN);
    }

    #[test]
    fn a_search_answers_the_best_catalogues_with_what_tells_them_apart() {
        let answer = search(&all(), &json!({"query": "solar orbiter reconnection"})).unwrap();
        assert_eq!(ids(&answer)[0], "public.b");
        let first = &answer["catalogues"][0];
        assert_eq!(first["name"], "Reconnection exhausts, Solar Orbiter");
        assert_eq!(first["folder"], "Literature/Heliosphere");
        assert_eq!(first["events"], 146);
        assert_eq!(first["changed"], "2026-11-13");
        assert!(first["description"].as_str().unwrap().contains("202346043"));
    }

    #[test]
    fn a_doi_answers_whether_the_paper_is_already_here() {
        let answer = search(&all(), &json!({"query": "10.1051/0004-6361/202346043"})).unwrap();
        assert_eq!(ids(&answer)[0], "public.b");
    }

    #[test]
    fn a_search_takes_a_limit_within_bounds_and_needs_a_query() {
        let one = search(&all(), &json!({"query": "solar", "limit": 1})).unwrap();
        assert_eq!(ids(&one).len(), 1);
        let zero = search(&all(), &json!({"query": "solar", "limit": 0})).unwrap();
        assert_eq!(ids(&zero).len(), 1, "at least one");
        assert!(search(&all(), &json!({"query": "  "})).is_err());
        assert!(search(&all(), &json!({})).is_err());
    }

    #[test]
    fn opening_finds_the_catalogue_by_id_or_says_to_search() {
        let all = all();
        assert_eq!(
            chosen(&all, &json!({"id": "public.c"})).unwrap().name,
            "Saturn reconnection"
        );
        let missing = chosen(&all, &json!({"id": "public.z"})).unwrap_err();
        assert!(missing.contains("search"), "{missing}");
        assert!(chosen(&all, &json!({})).is_err());
    }
}
