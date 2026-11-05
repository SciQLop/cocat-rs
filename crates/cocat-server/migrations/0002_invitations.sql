-- An invitation to join a room. The token itself is never stored: `token_hash` holds its
-- SHA-256 in hex, so a copy of the database cannot be turned into working links.
CREATE TABLE invitation (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    token_hash TEXT NOT NULL UNIQUE,
    room_id TEXT NOT NULL REFERENCES room (id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('editor', 'viewer')),
    email TEXT,
    created_by TEXT REFERENCES "user" (id) ON DELETE SET NULL,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    expires_at DATETIME NOT NULL,
    revoked BOOLEAN NOT NULL DEFAULT FALSE,
    max_uses INTEGER,
    uses INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX invitation_room ON invitation (room_id);
