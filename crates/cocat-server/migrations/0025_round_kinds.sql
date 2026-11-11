-- A round's kind picks its table of allowed moves; each return to proposing is a new revision;
-- a move can carry a note (the review conversation).
ALTER TABLE round ADD COLUMN kind TEXT NOT NULL DEFAULT 'labelling'
    CHECK (kind IN ('review', 'downlink', 'labelling'));
UPDATE round SET kind = 'downlink' WHERE budget IS NOT NULL;
ALTER TABLE round ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
ALTER TABLE round_log ADD COLUMN note TEXT;
