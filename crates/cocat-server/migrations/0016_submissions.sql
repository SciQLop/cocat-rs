-- Submissions to the public catalogues: a catalogue someone offers, as a new public
-- catalogue or as a change to one (`target`), until a curator accepts or declines it or its
-- author withdraws it. `snapshot` is the catalogue and its events as one Yjs update, taken
-- when it was submitted; `base` is how many changes the target had when the author copied
-- it, so a curator sees when it changed since.
CREATE TABLE submission (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    author TEXT NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('new', 'update')),
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
    snapshot BLOB NOT NULL
);
CREATE INDEX submission_by_author ON submission(author);
CREATE INDEX submission_by_status ON submission(status);

-- A catalogue copied out of a public catalogue to propose a change: which one, and how many
-- changes it had then. Kept on the server rather than in the catalogue, so it never travels
-- with the catalogue's content into a snapshot, an export or the public catalogue itself.
CREATE TABLE proposal (
    room_id TEXT NOT NULL REFERENCES room(id) ON DELETE CASCADE,
    catalogue TEXT NOT NULL,
    target TEXT NOT NULL REFERENCES room(id) ON DELETE CASCADE,
    base INTEGER NOT NULL,
    PRIMARY KEY (room_id, catalogue)
);
