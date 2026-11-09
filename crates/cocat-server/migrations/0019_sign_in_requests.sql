-- An app asking to sign in through the browser. The person compares `code` with what the
-- app shows and allows or denies it; the app polls with a secret whose hash is `poll_hash`
-- and, once allowed, receives a token (a named session) once, which deletes the row.
CREATE TABLE sign_in_request (
    poll_hash TEXT PRIMARY KEY,
    code TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved', 'denied')),
    user_id TEXT REFERENCES "user"(id) ON DELETE CASCADE,
    expires_at DATETIME NOT NULL,
    polled_at DATETIME
);
