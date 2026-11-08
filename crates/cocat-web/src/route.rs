//! What the page shows, kept in the URL's hash so a link opens the same view.

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Route {
    /// Nothing picked: the page shows the first catalogue.
    #[default]
    Default,
    Catalogue(String),
    Trash,
    /// The visitor's groups, and one of them.
    Groups,
    Group(String),
    /// The index of the public catalogues.
    Public,
    /// The curators' queue of submissions, the visitor's own, and one submission.
    Submissions,
    MySubmissions,
    Submission(i64),
    /// The versions of a catalogue, or of the room when `None`, and the one picked.
    History {
        catalogue: Option<String>,
        version: Option<u64>,
    },
}

impl Route {
    /// Reads a hash, with or without its `#`.
    pub fn parse(hash: &str) -> Self {
        let hash = hash.strip_prefix('#').unwrap_or(hash);
        if let Some(route) = history(hash) {
            return route;
        }
        match hash.split_once('=') {
            Some(("c", uuid)) if !uuid.is_empty() => Route::Catalogue(uuid.to_owned()),
            Some(("group", id)) if !id.is_empty() => Route::Group(id.to_owned()),
            Some(("submission", id)) => id.parse().map_or(Route::Default, Route::Submission),
            None if hash == "trash" => Route::Trash,
            None if hash == "groups" => Route::Groups,
            None if hash == "public" => Route::Public,
            None if hash == "submissions" => Route::Submissions,
            None if hash == "my-submissions" => Route::MySubmissions,
            _ => Route::Default,
        }
    }

    /// The view to show: the default view is the first catalogue, when there is one.
    pub fn or_first(self, first: Option<&str>) -> Self {
        match (self, first) {
            (Route::Default, Some(uuid)) => Route::Catalogue(uuid.to_owned()),
            (route, _) => route,
        }
    }

    /// The pages of the public catalogues: the index and the submissions.
    pub fn is_public_index(&self) -> bool {
        matches!(
            self,
            Route::Public | Route::Submissions | Route::MySubmissions | Route::Submission(_)
        )
    }

    /// The sidebar marks "Public catalogues" as current: on its pages, and anywhere in a
    /// public catalogue.
    pub fn in_public_catalogues(&self, public_project: bool) -> bool {
        public_project || self.is_public_index()
    }

    /// The history of what this view shows: its catalogue, else the whole room.
    pub fn history(&self) -> Self {
        let catalogue = match self {
            Route::Catalogue(uuid) => Some(uuid.clone()),
            Route::History { catalogue, .. } => catalogue.clone(),
            _ => None,
        };
        Route::History {
            catalogue,
            version: None,
        }
    }

    /// The live view a history view looks back on.
    pub fn live(&self) -> Self {
        match self {
            Route::History {
                catalogue: Some(uuid),
                ..
            } => Route::Catalogue(uuid.clone()),
            Route::History {
                catalogue: None, ..
            } => Route::Default,
            other => other.clone(),
        }
    }

    /// A link to this view of `page`. A bare `#…` would not do: the page's `<base>` is the
    /// app bundle's directory, which a fragment-only link resolves against.
    pub fn href(&self, page: &str) -> String {
        format!("{page}{}", self.hash())
    }

    pub fn hash(&self) -> String {
        match self {
            Route::Default => "#".to_owned(),
            Route::Catalogue(uuid) => format!("#c={uuid}"),
            Route::Trash => "#trash".to_owned(),
            Route::Groups => "#groups".to_owned(),
            Route::Public => "#public".to_owned(),
            Route::Submissions => "#submissions".to_owned(),
            Route::MySubmissions => "#my-submissions".to_owned(),
            Route::Submission(id) => format!("#submission={id}"),
            Route::Group(id) => format!("#group={id}"),
            Route::History { catalogue, version } => {
                let version = version.map(|version| format!("={version}"));
                let catalogue = catalogue.as_ref().map(|uuid| format!("&c={uuid}"));
                format!(
                    "#h{}{}",
                    version.unwrap_or_default(),
                    catalogue.unwrap_or_default()
                )
            }
        }
    }
}

/// The old project page sends here with `#share`: the page opens with its Share dialog.
pub fn opens_share(hash: &str) -> bool {
    hash.strip_prefix('#').unwrap_or(hash) == "share"
}

/// `h`, `h=<version>`, each with an optional `&c=<catalogue>`.
fn history(hash: &str) -> Option<Route> {
    let (head, catalogue) = match hash.split_once("&c=") {
        Some((_, "")) => return None,
        Some((head, uuid)) => (head, Some(uuid.to_owned())),
        None => (hash, None),
    };
    let version = match head {
        "h" => None,
        _ => Some(head.strip_prefix("h=")?.parse().ok()?),
    };
    Some(Route::History { catalogue, version })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_catalogue_is_named_by_its_uuid() {
        assert_eq!(Route::parse("c=abc"), Route::Catalogue("abc".into()));
        assert_eq!(Route::parse("#c=abc"), Route::Catalogue("abc".into()));
    }

    #[test]
    fn the_trash_has_its_own_route() {
        assert_eq!(Route::parse("trash"), Route::Trash);
    }

    #[test]
    fn the_groups_have_routes_of_their_own() {
        assert_eq!(Route::parse("#groups"), Route::Groups);
        assert_eq!(Route::parse("#group=g1"), Route::Group("g1".into()));
        assert_eq!(Route::parse("#group="), Route::Default);
        assert_eq!(Route::Groups.history(), history(None, None));
    }

    #[test]
    fn the_public_catalogues_have_a_route() {
        assert_eq!(Route::parse("#public"), Route::Public);
        assert_eq!(Route::Public.hash(), "#public");
        assert_eq!(Route::Public.history(), history(None, None));
    }

    #[test]
    fn the_submissions_have_routes() {
        assert_eq!(Route::parse("#submissions"), Route::Submissions);
        assert_eq!(Route::parse("#my-submissions"), Route::MySubmissions);
        assert_eq!(Route::parse("#submission=12"), Route::Submission(12));
        assert_eq!(Route::parse("#submission=x"), Route::Default);
        assert!(Route::Submission(12).is_public_index());
        assert!(Route::Public.is_public_index());
        assert!(!Route::Trash.is_public_index());
        assert_eq!(Route::Submissions.history(), history(None, None));
    }

    #[test]
    fn the_public_catalogues_are_current_on_their_pages_and_in_a_public_project() {
        assert!(Route::Public.in_public_catalogues(false));
        assert!(Route::Submission(3).in_public_catalogues(false));
        assert!(Route::Catalogue("abc".into()).in_public_catalogues(true));
        assert!(!Route::Catalogue("abc".into()).in_public_catalogues(false));
        assert!(!Route::Trash.in_public_catalogues(false));
    }

    #[test]
    fn share_opens_the_dialog_over_the_default_view() {
        assert!(opens_share("#share"));
        assert!(opens_share("share"));
        assert!(!opens_share("#c=share"));
        assert_eq!(Route::parse("#share"), Route::Default);
    }

    #[test]
    fn anything_else_is_the_default_view() {
        assert_eq!(Route::parse(""), Route::Default);
        assert_eq!(Route::parse("c="), Route::Default);
        assert_eq!(Route::parse("zz"), Route::Default);
    }

    #[test]
    fn the_default_view_is_the_first_catalogue() {
        assert_eq!(
            Route::Default.or_first(Some("abc")),
            Route::Catalogue("abc".into())
        );
        assert_eq!(Route::Default.or_first(None), Route::Default);
        assert_eq!(Route::Trash.or_first(Some("abc")), Route::Trash);
    }

    #[test]
    fn a_link_names_the_page_because_the_base_is_elsewhere() {
        let page = "/cocat-dev/ui/rooms/lab/browse";
        assert_eq!(
            Route::Trash.href(page),
            "/cocat-dev/ui/rooms/lab/browse#trash"
        );
    }

    fn history(catalogue: Option<&str>, version: Option<u64>) -> Route {
        Route::History {
            catalogue: catalogue.map(str::to_owned),
            version,
        }
    }

    #[test]
    fn the_history_names_its_version_and_catalogue() {
        assert_eq!(Route::parse("#h"), history(None, None));
        assert_eq!(Route::parse("#h=12"), history(None, Some(12)));
        assert_eq!(Route::parse("#h&c=abc"), history(Some("abc"), None));
        assert_eq!(Route::parse("#h=12&c=abc"), history(Some("abc"), Some(12)));
        assert_eq!(Route::parse("#h=x"), Route::Default);
        assert_eq!(Route::parse("#h&c="), Route::Default);
    }

    #[test]
    fn the_history_opens_on_what_is_on_screen_and_goes_back_to_it() {
        let catalogue = Route::Catalogue("abc".into());
        assert_eq!(catalogue.history(), history(Some("abc"), None));
        assert_eq!(Route::Trash.history(), history(None, None));
        assert_eq!(
            history(Some("abc"), Some(3)).history(),
            history(Some("abc"), None)
        );
        assert_eq!(history(Some("abc"), Some(3)).live(), catalogue);
        assert_eq!(history(None, Some(3)).live(), Route::Default);
        assert_eq!(Route::Trash.live(), Route::Trash);
    }

    #[test]
    fn a_route_writes_the_hash_it_reads() {
        for route in [
            Route::Catalogue("abc".into()),
            Route::Trash,
            Route::Groups,
            Route::Group("g1".into()),
            Route::Public,
            Route::Submissions,
            Route::MySubmissions,
            Route::Submission(7),
            history(None, None),
            history(Some("abc"), Some(12)),
            history(None, Some(12)),
            history(Some("abc"), None),
        ] {
            assert_eq!(Route::parse(&route.hash()), route);
        }
    }
}
