//! A search of every public catalogue, answered by the server (`GET /api/public/events`): the
//! rows it gives the table, and the names `catalogue:` takes in it.

use cocat_core::model::EventRecord;
use serde::Deserialize;

use crate::public::PublicCatalogue;
use crate::rows::{Row, public_row};

/// The most events a search shows; the heading says when there are more.
pub const LIMIT: usize = 10_000;

/// The server's answer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Found {
    pub events: Vec<PublicEvent>,
    pub total: usize,
    /// How many public catalogues the server left out: their index is still being written.
    #[serde(default)]
    pub indexing: usize,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PublicEvent {
    /// The id of the public catalogue holding it.
    pub catalogue: String,
    pub event: EventRecord,
}

/// The search's URL under `api`.
pub fn url(api: &str, query: &str) -> Option<String> {
    let mut url = url::Url::parse(&crate::projects::api_url(api, &["public", "events"])?).ok()?;
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("limit", &LIMIT.to_string());
    Some(url.into())
}

/// The (id, name) of each public catalogue, as a search names them.
pub fn names(public: &[PublicCatalogue]) -> Vec<(String, String)> {
    public
        .iter()
        .map(|catalogue| (catalogue.id.clone(), catalogue.name.clone()))
        .collect()
}

/// The table's rows, each naming its catalogue (by id when the index does not list it).
pub fn rows(found: Found, public: &[PublicCatalogue]) -> Vec<Row> {
    let name = |id: &str| {
        public
            .iter()
            .find(|catalogue| catalogue.id == id)
            .map_or_else(|| id.to_owned(), |catalogue| catalogue.name.clone())
    };
    found
        .events
        .into_iter()
        .map(|found| {
            let catalogue = name(&found.catalogue);
            public_row(found.event, catalogue)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use cocat_core::role::Role;
    use serde_json::json;

    use super::*;

    fn donki() -> PublicCatalogue {
        PublicCatalogue {
            id: "public.donki.cme".into(),
            name: "DONKI CMEs".into(),
            folder: None,
            source: None,
            description: None,
            role: Role::Viewer,
            changed: None,
            count: None,
            relay: None,
        }
    }

    #[test]
    fn the_search_goes_to_the_server_escaped() {
        assert_eq!(
            url("https://host/cocat/api", "tag:shock \"bow shock\"").as_deref(),
            Some("https://host/cocat/api/public/events?q=tag%3Ashock+%22bow+shock%22&limit=10000")
        );
    }

    #[test]
    fn each_row_names_its_public_catalogue() {
        let found: Found = serde_json::from_value(json!({
            "total": 2,
            "events": [
                {"catalogue": "public.donki.cme", "event": {"uuid": "a",
                 "start": "2020-01-01T00:00:00Z", "stop": "2020-01-01T01:00:00Z",
                 "tags": ["cme"], "attributes": {"speed": 900}}},
                {"catalogue": "public.gone", "event": {"uuid": "b",
                 "start": "2020-01-02T00:00:00Z", "stop": "2020-01-02T00:00:00Z"}},
            ],
        }))
        .unwrap();
        let rows = rows(found, &[donki()]);
        assert_eq!(rows[0].catalogue.as_deref(), Some("DONKI CMEs"));
        assert_eq!(rows[0].tags, ["cme"]);
        assert_eq!(rows[0].attributes["speed"], 900);
        assert_eq!(rows[0].stop - rows[0].start, 3_600_000_000_000);
        assert_eq!(rows[1].catalogue.as_deref(), Some("public.gone"));
    }
}
