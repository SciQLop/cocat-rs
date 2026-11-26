//! The filter box's search on a catalogue page: which side it is on and what the page lists.
//! What the box's text means, its colours and completions are
//! `cocat_core::query::search_box`, shared with the Python client.

pub use cocat_core::query::search_box::*;

use cocat_core::query;
use cocat_core::rules::{self, Set};
use serde_json::Value;

/// A catalogue's own search: its text, and whether it searches the public catalogues rather
/// than the project.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnSearch {
    pub text: String,
    pub public: bool,
}

/// `None` when the box cannot edit the search (not a catalogue, or a rule with time
/// operations), `Some(None)` when there is none yet.
pub type Own = Option<Option<OwnSearch>>;

/// A catalogue's own search, from its rule and the text it was `typed` as, shown as typed
/// while it still reads as the rule. `names` are the project's catalogues, `public` the
/// public ones; a search of a catalogue's events reads as a search of all with a
/// `catalogue:` clause, since the box has no other way to say it.
pub fn own_of(
    rule: Option<&Value>,
    typed: Option<&str>,
    names: &[(String, String)],
    public: &[(String, String)],
) -> Own {
    let Some(rule) = rule else {
        return Some(None);
    };
    let own = |text, public| Some(OwnSearch { text, public });
    let Ok(Set::Filter { filter, .. }) = rules::parse(rule) else {
        return None;
    };
    match *filter {
        Set::Public { .. } => query::where_text(rule, typed, public).map(|text| own(text, true)),
        Set::All { .. } => query::where_text(rule, typed, names).map(|text| own(text, false)),
        _ => query::rule_search(rule, names).map(|text| own(text, false)),
    }
}

/// What the box on a catalogue's page does: filter what the catalogue holds, or edit the
/// search that brings events into it, from the whole project or from every public catalogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Filter,
    Search,
    Public,
}

impl Side {
    /// Whether the box edits the catalogue's own search.
    pub fn searches(self) -> bool {
        self != Side::Filter
    }
}

/// What a catalogue page lists before the box filters it, from the project; the public
/// catalogues' part is [`public_query`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listing {
    /// What the catalogue holds now.
    Members,
    /// What it would hold with the box's search: its picked events and what the search finds.
    Preview,
    /// Only its picked events: the box searches the public catalogues.
    Picked,
}

/// The catalogue's own search, if it has one: its text, and whether it searches the public
/// catalogues.
pub type Saved<'s> = Option<(&'s str, bool)>;

pub fn listing(side: Side, saved: Saved<'_>, text: &str) -> Listing {
    let text = text.trim();
    match side {
        Side::Filter => Listing::Members,
        Side::Search if saved == Some((text, false)) || (saved.is_none() && text.is_empty()) => {
            Listing::Members
        }
        Side::Search => Listing::Preview,
        Side::Public => Listing::Picked,
    }
}

/// The search the server runs through the public catalogues for the page, if any: the box's
/// on that side, else the catalogue's own when it searches them. A box with a `mistake` asks
/// nothing: a search the server would refuse or read otherwise gives no count to show.
pub fn public_query(side: Side, saved: Saved<'_>, text: &str, mistake: bool) -> Option<String> {
    let text = text.trim();
    match (side, saved) {
        (Side::Public, _) => (!text.is_empty() && !mistake).then(|| text.to_owned()),
        (Side::Filter, Some((saved, true))) => Some(saved.to_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod listing_tests {
    use super::{Listing, Side, listing, public_query};

    #[test]
    fn a_filter_never_changes_what_the_catalogue_lists() {
        assert_eq!(
            listing(Side::Filter, Some(("tag:x", false)), "tag:y"),
            Listing::Members
        );
        assert_eq!(listing(Side::Filter, None, "tag:y"), Listing::Members);
    }

    #[test]
    fn its_search_as_saved_lists_what_it_holds() {
        assert_eq!(
            listing(Side::Search, Some(("tag:x", false)), " tag:x "),
            Listing::Members
        );
        assert_eq!(listing(Side::Search, None, " "), Listing::Members);
    }

    #[test]
    fn an_edited_search_previews_what_it_would_hold() {
        assert_eq!(
            listing(Side::Search, Some(("tag:x", false)), "tag:y"),
            Listing::Preview
        );
        assert_eq!(listing(Side::Search, None, "tag:y"), Listing::Preview);
        assert_eq!(
            listing(Side::Search, Some(("tag:x", false)), ""),
            Listing::Preview
        );
        assert_eq!(
            listing(Side::Search, Some(("tag:x", true)), "tag:x"),
            Listing::Preview,
            "the same words through the project are another search"
        );
    }

    #[test]
    fn the_public_catalogues_are_searched_by_the_server_beside_the_picked_events() {
        assert_eq!(listing(Side::Public, None, "tag:x"), Listing::Picked);
        assert_eq!(
            public_query(Side::Public, None, " tag:x ", false).as_deref(),
            Some("tag:x")
        );
        assert_eq!(public_query(Side::Public, None, " ", false), None);
        assert_eq!(
            public_query(Side::Search, Some(("tag:x", true)), "tag:y", false),
            None
        );
    }

    #[test]
    fn a_text_with_a_mistake_asks_the_server_nothing() {
        let named = r#"start:>2024 catalogue:"jet candidates""#;
        assert_eq!(public_query(Side::Public, None, named, true), None);
        let saved = Some(("tag:x", true));
        assert_eq!(
            public_query(Side::Filter, saved, "tag:(", true).as_deref(),
            Some("tag:x"),
            "a filter's mistake leaves the catalogue's own search"
        );
    }

    #[test]
    fn a_catalogue_searching_them_holds_what_the_server_finds_under_any_filter() {
        let saved = Some(("tag:x", true));
        assert_eq!(
            public_query(Side::Filter, saved, "wave", false).as_deref(),
            Some("tag:x")
        );
        assert_eq!(
            public_query(Side::Filter, Some(("tag:x", false)), "", false),
            None
        );
    }
}
