-- A round's candidates are a proposal written once by the server, at creation, from a
-- catalogue the creator can read. Its author is the round's creator, who may also have a
-- proposal of their own, so the flag joins the primary key; a round has one at most.
CREATE TABLE round_proposal_new (
    round_id TEXT NOT NULL REFERENCES round(id) ON DELETE CASCADE,
    author TEXT NOT NULL,
    doc_id TEXT NOT NULL UNIQUE,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    candidates INTEGER NOT NULL DEFAULT 0 CHECK (candidates IN (0, 1)),
    source_project TEXT,
    source_catalogue TEXT,
    candidate_count INTEGER,
    PRIMARY KEY (round_id, author, candidates)
);

INSERT INTO round_proposal_new (round_id, author, doc_id, created_at)
    SELECT round_id, author, doc_id, created_at FROM round_proposal;

DROP TABLE round_proposal;

ALTER TABLE round_proposal_new RENAME TO round_proposal;

CREATE UNIQUE INDEX round_one_candidates ON round_proposal (round_id) WHERE candidates = 1;
