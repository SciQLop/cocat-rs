-- When each member last opened each project (a socket accepted for it), so the home page
-- tells what others changed since.
CREATE TABLE room_seen (
    user_id TEXT NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    room_id TEXT NOT NULL REFERENCES room(id) ON DELETE CASCADE,
    seen_ms INTEGER NOT NULL,
    PRIMARY KEY (user_id, room_id)
);
