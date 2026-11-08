-- The index of public catalogues shows how many events each holds. Counting means loading the
-- room, so the count is kept with the room file it was taken from (size and modification
-- time): the index counts again only after that file changed.
ALTER TABLE room ADD COLUMN public_count INTEGER;
ALTER TABLE room ADD COLUMN counted_file TEXT;
