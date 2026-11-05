-- A group of users that can be given rooms as a whole. `group` is an SQL keyword, hence
-- `user_group`. The owner is a row in `group_member`, guarded by a partial unique index so
-- a group can never have two.
CREATE TABLE user_group (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    created_by TEXT REFERENCES "user" (id) ON DELETE SET NULL
);

CREATE TABLE group_member (
    group_id TEXT NOT NULL REFERENCES user_group (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES "user" (id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('owner', 'member')),
    joined_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (group_id, user_id)
);

CREATE UNIQUE INDEX one_owner_per_group ON group_member (group_id) WHERE role = 'owner';

CREATE INDEX group_member_user ON group_member (user_id);

-- A room shared with a group. Never `owner`: the owner role only exists as a direct
-- membership, so a group grant can never hand the room over.
CREATE TABLE room_group (
    room_id TEXT NOT NULL REFERENCES room (id) ON DELETE CASCADE,
    group_id TEXT NOT NULL REFERENCES user_group (id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('editor', 'viewer')),
    PRIMARY KEY (room_id, group_id)
);

CREATE INDEX room_group_group ON room_group (group_id);
