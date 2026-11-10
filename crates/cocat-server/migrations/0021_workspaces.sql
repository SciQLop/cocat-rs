-- Projects are catalogue projects (one catalogues document) or workspaces (a manifest plus
-- notebook, text and file documents). Documents and blobs belong to their project.
ALTER TABLE room ADD COLUMN kind TEXT NOT NULL DEFAULT 'catalogues'
    CHECK (kind IN ('catalogues', 'workspace'));

CREATE TABLE document (
    room_id TEXT NOT NULL REFERENCES room(id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('manifest', 'notebook', 'text')),
    created_by TEXT REFERENCES "user"(id) ON DELETE SET NULL,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (room_id, id)
);

CREATE TABLE blob (
    room_id TEXT NOT NULL REFERENCES room(id) ON DELETE CASCADE,
    sha256 TEXT NOT NULL,
    size INTEGER NOT NULL,
    created_by TEXT REFERENCES "user"(id) ON DELETE SET NULL,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (room_id, sha256)
);
