-- An invitation to join a group. Like `invitation`, only the token's SHA-256 is stored, and
-- there is no role column: a group invitation always admits its bearer as a member.
CREATE TABLE group_invitation (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    token_hash TEXT NOT NULL UNIQUE,
    group_id TEXT NOT NULL REFERENCES user_group (id) ON DELETE CASCADE,
    email TEXT,
    created_by TEXT REFERENCES "user" (id) ON DELETE SET NULL,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    expires_at DATETIME NOT NULL,
    revoked BOOLEAN NOT NULL DEFAULT FALSE,
    max_uses INTEGER,
    uses INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX group_invitation_group ON group_invitation (group_id);
