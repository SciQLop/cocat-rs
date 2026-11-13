-- The banner every page shows, set by an administrator on the overview page. One row at most:
-- no row means no banner.
CREATE TABLE server_notice (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    text TEXT NOT NULL
);
