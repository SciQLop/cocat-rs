-- The attribute keys a public catalogue's events carry, as a JSON list, read with its count
-- from the file `counted_file` names: a search of the public catalogues refuses a name that
-- is neither a field nor one of them. NULL until the next count.
ALTER TABLE room ADD COLUMN public_attributes TEXT;
