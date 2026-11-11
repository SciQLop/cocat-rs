-- A reviewer gives each event a verdict, tagged with the round revision it was given in.
-- reviewer is not a foreign key, like a proposal's author: deleting an account keeps its verdicts.
CREATE TABLE round_verdict (
    round_id TEXT NOT NULL REFERENCES round(id) ON DELETE CASCADE,
    reviewer TEXT NOT NULL,
    event TEXT NOT NULL,
    verdict TEXT NOT NULL CHECK (verdict IN ('accept', 'reject', 'unsure')),
    revision INTEGER NOT NULL,
    at DATETIME DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (round_id, reviewer, event)
);

INSERT INTO round_verdict (round_id, reviewer, event, verdict, revision)
    SELECT round_id, voter, event, 'accept', 1 FROM round_vote;

DROP TABLE round_vote;
