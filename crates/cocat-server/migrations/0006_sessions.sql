-- Server-side sessions: the cookie holds an opaque random token, and only its hash is kept,
-- so a logout or a password change can take the session away before it expires.
CREATE TABLE session (
    token_hash TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    expires_at DATETIME NOT NULL
);

CREATE INDEX session_user ON session(user_id);
