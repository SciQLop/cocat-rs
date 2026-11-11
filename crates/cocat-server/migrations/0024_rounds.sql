-- A selection round: members of a project each propose intervals, optionally vote, and a
-- decider writes the final catalogue. Proposals are documents of the project.
CREATE TABLE round (
    id TEXT PRIMARY KEY,
    room_id TEXT NOT NULL REFERENCES room(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'proposing' CHECK (state IN ('proposing', 'voting', 'decided')),
    visibility TEXT NOT NULL CHECK (visibility IN ('open', 'blind')),
    deadline TEXT,
    budget TEXT,
    final_catalogue TEXT,
    created_by TEXT REFERENCES "user"(id) ON DELETE SET NULL,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE round_member (
    round_id TEXT NOT NULL REFERENCES round(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('proposer', 'voter', 'decider')),
    PRIMARY KEY (round_id, user_id, role)
);

CREATE TABLE round_proposal (
    round_id TEXT NOT NULL REFERENCES round(id) ON DELETE CASCADE,
    author TEXT NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    doc_id TEXT NOT NULL UNIQUE,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (round_id, author)
);

CREATE TABLE round_vote (
    round_id TEXT NOT NULL REFERENCES round(id) ON DELETE CASCADE,
    voter TEXT NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    event TEXT NOT NULL,
    PRIMARY KEY (round_id, voter, event)
);

CREATE TABLE round_log (
    round_id TEXT NOT NULL REFERENCES round(id) ON DELETE CASCADE,
    at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    actor TEXT REFERENCES "user"(id) ON DELETE SET NULL,
    what TEXT NOT NULL
);
