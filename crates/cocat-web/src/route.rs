//! What the page shows, kept in the URL's hash so a link opens the same view.

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Route {
    /// Nothing picked: the page shows the first catalogue.
    #[default]
    Default,
    Catalogue(String),
    Trash,
}

impl Route {
    /// Reads a hash, with or without its `#`.
    pub fn parse(hash: &str) -> Self {
        let hash = hash.strip_prefix('#').unwrap_or(hash);
        match hash.split_once('=') {
            Some(("c", uuid)) if !uuid.is_empty() => Route::Catalogue(uuid.to_owned()),
            None if hash == "trash" => Route::Trash,
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

    pub fn hash(&self) -> String {
        match self {
            Route::Default => "#".to_owned(),
            Route::Catalogue(uuid) => format!("#c={uuid}"),
            Route::Trash => "#trash".to_owned(),
        }
    }
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
    fn a_route_writes_the_hash_it_reads() {
        for route in [Route::Catalogue("abc".into()), Route::Trash] {
            assert_eq!(Route::parse(&route.hash()), route);
        }
    }
}
