//! What the page shows, kept in the URL's hash so a link opens the same view.

use url::form_urlencoded;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Route {
    /// Nothing picked: the page shows the first catalogue.
    #[default]
    Default,
    /// What needs the visitor: invitations, reviews, and what changed in their projects.
    Home,
    /// Every event of the open project, where a search covers the whole project.
    AllEvents,
    Catalogue(String),
    Trash,
    /// The visitor's groups, and one of them.
    Groups,
    Group(String),
    /// The index of the public catalogues: all of them, or one folder's (`AMDA/EARTH`).
    Public(Option<String>),
    /// The curators' queue of submissions, the visitor's own, and one submission.
    Submissions,
    MySubmissions,
    Submission(i64),
    /// The versions of a catalogue, or of the room when `None`, and the one picked.
    History {
        catalogue: Option<String>,
        version: Option<u64>,
    },
    /// The selection rounds of the open project, and one of them by id.
    Rounds,
    NewRound,
    Round(String),
    /// The visitor's own proposal in a round, by the round's id.
    Proposal(String),
    /// A round's review table, with the event of the row picked.
    Review {
        id: String,
        row: Option<String>,
    },
    /// The decider's page that builds a round's final catalogue.
    Decide(String),
    /// A project's file, by its path in the file list.
    File(String),
    /// A catalogue of another project this one links to, by the link's id.
    Link(String),
}

impl Route {
    /// Reads a hash, with or without its `#`.
    pub fn parse(hash: &str) -> Self {
        let hash = hash.strip_prefix('#').unwrap_or(hash);
        if let Some(route) = history(hash) {
            return route;
        }
        if let Some(folder) = hash.strip_prefix("public=") {
            return Route::Public(decoded(folder));
        }
        if let Some(path) = hash.strip_prefix("file=") {
            return decoded(path).map_or(Route::Default, Route::File);
        }
        if let Some(id) = hash.strip_prefix("/rounds/") {
            if let Some(route) = round_page(id) {
                return route;
            }
            return if id.is_empty() {
                Route::Rounds
            } else if id == "new" {
                Route::NewRound
            } else {
                Route::Round(id.to_owned())
            };
        }
        match hash.split_once('=') {
            Some(("c", uuid)) if !uuid.is_empty() => Route::Catalogue(uuid.to_owned()),
            Some(("group", id)) if !id.is_empty() => Route::Group(id.to_owned()),
            Some(("link", id)) if !id.is_empty() => Route::Link(id.to_owned()),
            Some(("submission", id)) => id.parse().map_or(Route::Default, Route::Submission),
            None if hash == "/rounds" => Route::Rounds,
            None if hash == "home" => Route::Home,
            None if hash == "all" => Route::AllEvents,
            None if hash == "trash" => Route::Trash,
            None if hash == "groups" => Route::Groups,
            None if hash == "public" => Route::Public(None),
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
            Route::Public(_) | Route::Submissions | Route::MySubmissions | Route::Submission(_)
        )
    }

    /// Pages that hold no project: the public catalogues' own pages and the home page. A
    /// project picked from one of them opens even when it is the last one open.
    pub fn holds_no_project(&self) -> bool {
        *self == Route::Home || self.is_public_index()
    }

    /// The page is in the public catalogues, which the switcher then names: on their pages,
    /// and anywhere in a public catalogue.
    pub fn in_public_catalogues(&self, public_project: bool) -> bool {
        *self != Route::Home && (public_project || self.is_public_index())
    }

    /// What the header's breadcrumb names after the project switcher. The public index has
    /// none: the switcher already reads "Public catalogues".
    pub fn crumb(&self) -> Option<&'static str> {
        match self {
            Route::Home => Some("Home"),
            Route::History { .. } => Some("History"),
            Route::Groups | Route::Group(_) => Some("Groups"),
            route if route.in_rounds() => Some("Rounds"),
            Route::Submissions | Route::Submission(_) => Some("Submissions"),
            Route::MySubmissions => Some("My submissions"),
            _ => None,
        }
    }

    /// The pages about selection rounds.
    pub fn in_rounds(&self) -> bool {
        matches!(
            self,
            Route::Rounds
                | Route::NewRound
                | Route::Round(_)
                | Route::Proposal(_)
                | Route::Review { .. }
                | Route::Decide(_)
        )
    }

    /// The page this view is on, whichever row it picked.
    pub fn page(&self) -> Self {
        match self {
            Route::Review { id, .. } => Route::Review {
                id: id.clone(),
                row: None,
            },
            other => other.clone(),
        }
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
            Route::Home => "#home".to_owned(),
            Route::AllEvents => "#all".to_owned(),
            Route::Trash => "#trash".to_owned(),
            Route::Groups => "#groups".to_owned(),
            Route::Rounds => "#/rounds".to_owned(),
            Route::NewRound => "#/rounds/new".to_owned(),
            Route::Round(id) => format!("#/rounds/{id}"),
            Route::Proposal(id) => format!("#/rounds/{id}/proposal"),
            Route::Review { id, row: None } => format!("#/rounds/{id}/review"),
            Route::Review { id, row: Some(row) } => format!("#/rounds/{id}/review?row={row}"),
            Route::Decide(id) => format!("#/rounds/{id}/decide"),
            Route::Public(None) => "#public".to_owned(),
            Route::Public(Some(folder)) => {
                format!(
                    "#public={}",
                    form_urlencoded::byte_serialize(folder.as_bytes()).collect::<String>()
                )
            }
            Route::File(path) => format!(
                "#file={}",
                form_urlencoded::byte_serialize(path.as_bytes()).collect::<String>()
            ),
            Route::Submissions => "#submissions".to_owned(),
            Route::MySubmissions => "#my-submissions".to_owned(),
            Route::Submission(id) => format!("#submission={id}"),
            Route::Group(id) => format!("#group={id}"),
            Route::Link(id) => format!("#link={id}"),
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

/// A folder or path escaped as a form value; an empty one is none.
fn decoded(folder: &str) -> Option<String> {
    let text: String = form_urlencoded::parse(format!("f={folder}").as_bytes())
        .map(|(_, value)| value.into_owned())
        .collect();
    (!text.is_empty()).then_some(text)
}

/// `<round>/proposal`, `<round>/review[?row=<event>]` and `<round>/decide`.
fn round_page(path: &str) -> Option<Route> {
    let (id, page) = path.split_once('/').filter(|(id, _)| !id.is_empty())?;
    let id = id.to_owned();
    match page.split_once("?row=") {
        Some(("review", row)) => Some(Route::Review {
            id,
            row: (!row.is_empty()).then(|| row.to_owned()),
        }),
        _ => match page {
            "proposal" => Some(Route::Proposal(id)),
            "review" => Some(Route::Review { id, row: None }),
            "decide" => Some(Route::Decide(id)),
            _ => None,
        },
    }
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
    fn a_file_route_round_trips() {
        for path in [
            "notebooks/analysis.ipynb",
            "My notes/a b#c.md",
            "über/ü.py",
            "a&b=c.txt",
        ] {
            let route = Route::File(path.to_owned());
            assert_eq!(Route::parse(&route.hash()), route, "{path}");
        }
        assert_eq!(Route::parse("file="), Route::Default);
    }

    #[test]
    fn a_link_is_named_by_its_id() {
        assert_eq!(Route::parse("#link=l1"), Route::Link("l1".into()));
        assert_eq!(Route::Link("l1".into()).hash(), "#link=l1");
        assert_eq!(Route::parse("#link="), Route::Default);
        assert_eq!(Route::Link("l1".into()).history(), history(None, None));
    }

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
    fn the_new_round_route_round_trips() {
        assert_eq!(Route::parse("#/rounds/new"), Route::NewRound);
        assert_eq!(Route::NewRound.hash(), "#/rounds/new");
        assert_eq!(Route::NewRound.crumb(), Some("Rounds"));
    }

    #[test]
    fn the_route_round_trips_through_the_hash() {
        assert_eq!(Route::parse("#/rounds"), Route::Rounds);
        assert_eq!(Route::parse("#/rounds/r1"), Route::Round("r1".into()));
        assert_eq!(Route::parse("#/rounds/"), Route::Rounds);
        assert_eq!(Route::Round("r1".into()).hash(), "#/rounds/r1");
        assert_eq!(Route::Round("r1".into()).crumb(), Some("Rounds"));
    }

    #[test]
    fn the_public_catalogues_have_a_route() {
        assert_eq!(Route::parse("#public"), Route::Public(None));
        assert_eq!(Route::Public(None).hash(), "#public");
        assert_eq!(Route::Public(None).history(), history(None, None));
    }

    #[test]
    fn a_folder_of_the_public_catalogues_is_in_the_hash() {
        let folder = |path: &str| Route::Public(Some(path.to_owned()));
        assert_eq!(Route::parse("#public="), Route::Public(None));
        for path in ["AMDA/EARTH", "Solar wind & co/été", "a=b#c"] {
            let hash = folder(path).hash();
            assert!(!hash[1..].contains('#'), "{hash}");
            assert_eq!(Route::parse(&hash), folder(path));
        }
        assert_eq!(folder("AMDA").hash(), "#public=AMDA");
        assert!(folder("AMDA").is_public_index());
    }

    #[test]
    fn the_breadcrumb_names_the_pages_the_switcher_does_not() {
        assert_eq!(Route::Public(None).crumb(), None);
        assert_eq!(Route::Public(Some("AMDA".into())).crumb(), None);
        assert_eq!(Route::Submission(3).crumb(), Some("Submissions"));
        assert_eq!(Route::MySubmissions.crumb(), Some("My submissions"));
        assert_eq!(Route::Group("g".into()).crumb(), Some("Groups"));
        assert_eq!(history(None, None).crumb(), Some("History"));
        assert_eq!(Route::Trash.crumb(), None);
    }

    #[test]
    fn the_submissions_have_routes() {
        assert_eq!(Route::parse("#submissions"), Route::Submissions);
        assert_eq!(Route::parse("#my-submissions"), Route::MySubmissions);
        assert_eq!(Route::parse("#submission=12"), Route::Submission(12));
        assert_eq!(Route::parse("#submission=x"), Route::Default);
        assert!(Route::Submission(12).is_public_index());
        assert!(Route::Public(None).is_public_index());
        assert!(!Route::Trash.is_public_index());
        assert!(Route::Home.holds_no_project());
        assert!(Route::Submissions.holds_no_project());
        assert!(!Route::Trash.holds_no_project());
        assert!(!Route::Home.in_public_catalogues(true));
        assert_eq!(Route::Submissions.history(), history(None, None));
    }

    #[test]
    fn the_public_catalogues_are_current_on_their_pages_and_in_a_public_project() {
        assert!(Route::Public(None).in_public_catalogues(false));
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
    fn the_proposal_route_round_trips() {
        let route = Route::parse("#/rounds/r1/proposal");
        assert_eq!(route, Route::Proposal("r1".into()));
        assert_eq!(route.hash(), "#/rounds/r1/proposal");
        assert_eq!(route.crumb(), Some("Rounds"));
    }

    #[test]
    fn the_review_and_decide_routes_round_trip() {
        let review = |row: Option<&str>| Route::Review {
            id: "r1".into(),
            row: row.map(str::to_owned),
        };
        assert_eq!(Route::parse("#/rounds/r1/review"), review(None));
        assert_eq!(
            Route::parse("#/rounds/r1/review?row=e7"),
            review(Some("e7"))
        );
        assert_eq!(review(Some("e7")).hash(), "#/rounds/r1/review?row=e7");
        assert_eq!(Route::parse("#/rounds/r1/review?row="), review(None));
        assert_eq!(review(Some("e7")).page(), review(None));
        assert_eq!(Route::Trash.page(), Route::Trash);
        assert_eq!(
            Route::parse("#/rounds/r1/decide"),
            Route::Decide("r1".into())
        );
        assert_eq!(Route::Decide("r1".into()).hash(), "#/rounds/r1/decide");
        for route in [review(None), Route::Decide("r1".into())] {
            assert_eq!(route.crumb(), Some("Rounds"));
            assert!(route.in_rounds());
        }
        assert!(!Route::Trash.in_rounds());
    }

    #[test]
    fn a_route_writes_the_hash_it_reads() {
        for route in [
            Route::Review {
                id: "r1".into(),
                row: Some("e1".into()),
            },
            Route::Decide("r1".into()),
            Route::Home,
            Route::AllEvents,
            Route::Catalogue("abc".into()),
            Route::Trash,
            Route::Groups,
            Route::Group("g1".into()),
            Route::Rounds,
            Route::NewRound,
            Route::Round("r1".into()),
            Route::Proposal("r1".into()),
            Route::Public(None),
            Route::Public(Some("AMDA/EARTH".into())),
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
