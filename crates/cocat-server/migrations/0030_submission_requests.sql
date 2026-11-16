-- A request: someone asks the curators to add a list they found (a paper's table, a
-- supplement, a web page). It names the list (`title`) and where it is (`description`), with
-- no catalogue and an empty snapshot; accepting it records the answer, it publishes nothing.
-- SQLite cannot change a CHECK in place, so the table is rebuilt with every row kept.
CREATE TABLE submission_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    author TEXT NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('new', 'update', 'request')),
    target TEXT REFERENCES room(id) ON DELETE SET NULL,
    base INTEGER,
    catalogue TEXT NOT NULL,
    title TEXT NOT NULL,
    folder TEXT,
    description TEXT,
    note TEXT,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'accepted', 'declined', 'withdrawn')),
    reviewer TEXT REFERENCES "user"(id) ON DELETE SET NULL,
    review_note TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    decided_at TEXT,
    snapshot BLOB NOT NULL,
    room TEXT REFERENCES room(id) ON DELETE SET NULL
);
INSERT INTO submission_new
    (id, author, kind, target, base, catalogue, title, folder, description, note, status,
     reviewer, review_note, created_at, decided_at, snapshot, room)
SELECT id, author, kind, target, base, catalogue, title, folder, description, note, status,
       reviewer, review_note, created_at, decided_at, snapshot, room
FROM submission;
DROP TABLE submission;
ALTER TABLE submission_new RENAME TO submission;
CREATE INDEX submission_by_author ON submission(author);
CREATE INDEX submission_by_status ON submission(status);
