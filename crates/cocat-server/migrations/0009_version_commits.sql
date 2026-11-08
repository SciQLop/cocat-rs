-- Versions people saved on purpose. `record` is the room's last record when they saved: the
-- version that holds it ends there, whatever comes next.
CREATE TABLE version_commit (
    room_id TEXT NOT NULL REFERENCES room(id) ON DELETE CASCADE,
    record INTEGER NOT NULL,
    message TEXT,
    created_by TEXT REFERENCES "user"(id) ON DELETE SET NULL,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (room_id, record)
);
