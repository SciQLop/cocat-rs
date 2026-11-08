-- The names people give versions of a room's history. A version is its last record's index.
CREATE TABLE version_name (
    room_id TEXT NOT NULL REFERENCES room(id) ON DELETE CASCADE,
    version INTEGER NOT NULL,
    name TEXT NOT NULL,
    created_by TEXT REFERENCES "user"(id) ON DELETE SET NULL,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (room_id, version)
);
