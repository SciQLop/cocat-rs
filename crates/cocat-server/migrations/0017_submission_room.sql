-- The project a submission was taken from, so accepting an update moves that copy's base:
-- the copy then holds what was accepted, and its next update is not stale.
ALTER TABLE submission ADD COLUMN room TEXT REFERENCES room(id) ON DELETE SET NULL;
