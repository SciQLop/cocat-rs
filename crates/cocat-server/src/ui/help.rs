//! The help page: what projects, catalogues, history and sharing are, for a newcomer.

use askama::Template;
use axum::extract::State;
use axum::response::Response;

use crate::AppState;
use crate::ui::frame::{self, Frame, MaybeUser};

/// What the Python example connects to when the server does not know its public address.
const UNKNOWN_SERVER: &str = "https://your-cocat-server";

/// Smart-catalogue rules the page shows, each with what it picks. A test parses every one.
const RULE_EXAMPLES: [(&str, &str); 4] = [
    (
        "Every event tagged “shock”",
        r#"{"filter": {"all": true},
 "where": {"contains": ["tags", "shock"]}}"#,
    ),
    (
        "Reviewed events with a confidence of 0.8 or more",
        r#"{"filter": {"all": true},
 "where": {"and": [{"cmp": ["status", "==", "reviewed"]},
                   {"cmp": ["confidence", ">=", 0.8]}]}}"#,
    ),
    (
        "Events since 2020 whose “spacecraft” attribute is MMS1",
        r##"{"filter": {"all": true},
 "where": {"and": [{"cmp": ["start", ">=", "2020-01-01T00:00:00Z"]},
                   {"cmp": ["#spacecraft", "==", "MMS1"]}]}}"##,
    ),
    (
        "Shock events less than 10 minutes apart, joined into one interval",
        r#"{"merge": {"filter": {"all": true},
           "where": {"contains": ["tags", "shock"]}},
 "gap": 600}"#,
    ),
];

#[derive(Template)]
#[template(path = "help.html")]
struct Help {
    frame: Frame,
    server_url: String,
    rules: [(&'static str, &'static str); 4],
    groups_href: String,
    account_href: String,
}

pub(crate) async fn page(State(state): State<AppState>, MaybeUser(user): MaybeUser) -> Response {
    let links = &state.links;
    frame::page(Help {
        frame: Frame::new(links, user.as_ref(), "Help"),
        server_url: links
            .public_base()
            .unwrap_or_else(|| UNKNOWN_SERVER.to_owned()),
        rules: RULE_EXAMPLES,
        groups_href: links.path("/groups"),
        account_href: links.path("/account"),
    })
}

#[cfg(test)]
mod tests {
    use super::RULE_EXAMPLES;

    #[test]
    fn every_rule_example_is_a_valid_rule() {
        for (what, text) in RULE_EXAMPLES {
            let rule: serde_json::Value = serde_json::from_str(text).expect(what);
            if let Err(error) = cocat_core::rules::parse(&rule) {
                panic!("{what}: {error}");
            }
        }
    }
}
