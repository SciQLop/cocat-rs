-- A named session is a token an app signs in with ("Python on lpp-jupyter"): it lasts until
-- revoked or six months unused. A browser session has no name. `id` names a session in the
-- API and on the account page, so the token itself is never shown again.
ALTER TABLE session ADD COLUMN id TEXT;
ALTER TABLE session ADD COLUMN name TEXT;
ALTER TABLE session ADD COLUMN created_at DATETIME;
ALTER TABLE session ADD COLUMN last_used_at DATETIME;

UPDATE session
SET id = lower(hex(randomblob(16))),
    created_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
    last_used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now');

CREATE UNIQUE INDEX session_id ON session(id);
