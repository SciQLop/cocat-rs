-- Catalogue providers: remote event lists the server relays into public catalogues. Every
-- provider is off until the administrator turns it on. A relayed public catalogue is made by
-- the server, so its room has no `created_by` (the column already accepts NULL).
CREATE TABLE relay_provider (
    id TEXT PRIMARY KEY,
    enabled BOOLEAN NOT NULL DEFAULT 0,
    schedule TEXT,
    settings TEXT NOT NULL DEFAULT '{}',
    running BOOLEAN NOT NULL DEFAULT 0,
    last_run_at TEXT,
    last_ok_at TEXT,
    last_result TEXT
);

CREATE TABLE relay_catalogue (
    room_id TEXT PRIMARY KEY REFERENCES room(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    key TEXT NOT NULL,
    published BOOLEAN NOT NULL DEFAULT 1,
    refreshed_at TEXT,
    url TEXT
);
