//! Where the catalogue browser points a newcomer: the help page and what an empty list says.

/// The help page, relative to the app's `<base>` as the other server links are, so a proxy
/// prefix carries over.
pub const PAGE: &str = "../help";

/// One section of the help page.
pub fn href(anchor: &str) -> String {
    format!("{PAGE}#{anchor}")
}

/// What the table is showing, as far as its empty state cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    NoCatalogue,
    Catalogue { smart: bool },
    Trash,
}

/// What an empty table says: why it is empty and, for an editor, what to do next.
pub fn empty_text(shown: Shown, filtered: bool, can_write: bool) -> &'static str {
    match (shown, filtered, can_write) {
        (_, true, _) => "No event matches the filter.",
        (Shown::NoCatalogue, _, true) => {
            "This project has no catalogue yet. Make one with the + next to Catalogues, or from SciQLop."
        }
        (Shown::NoCatalogue, _, false) => "This project has no catalogue yet.",
        (Shown::Catalogue { smart: true }, _, _) => "The rule picks no event right now.",
        (Shown::Catalogue { smart: false }, _, true) => {
            "No events yet. Add one with New event, or mark intervals in SciQLop."
        }
        (Shown::Catalogue { smart: false }, _, false) => "No events in this catalogue yet.",
        (Shown::Trash, _, _) => "The trash is empty.",
    }
}

#[cfg(test)]
mod tests {
    use super::{Shown, empty_text, href};

    #[test]
    fn a_section_link_climbs_out_of_the_app() {
        assert_eq!(href("history"), "../help#history");
    }

    #[test]
    fn a_filter_that_hides_everything_says_so() {
        for shown in [
            Shown::NoCatalogue,
            Shown::Catalogue { smart: false },
            Shown::Trash,
        ] {
            assert_eq!(
                empty_text(shown, true, true),
                "No event matches the filter."
            );
        }
    }

    #[test]
    fn an_editor_is_told_what_to_do_next() {
        assert!(empty_text(Shown::NoCatalogue, false, true).contains("+"));
        assert!(empty_text(Shown::Catalogue { smart: false }, false, true).contains("New event"));
    }

    #[test]
    fn a_viewer_is_not_offered_buttons_they_lack() {
        assert!(!empty_text(Shown::NoCatalogue, false, false).contains("+"));
        assert!(!empty_text(Shown::Catalogue { smart: false }, false, false).contains("New event"));
    }

    #[test]
    fn an_empty_smart_catalogue_blames_its_rule() {
        assert_eq!(
            empty_text(Shown::Catalogue { smart: true }, false, true),
            "The rule picks no event right now."
        );
        assert_eq!(empty_text(Shown::Trash, false, true), "The trash is empty.");
    }
}
