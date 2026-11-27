-- The values of a public catalogue's text attributes, as a JSON map from key to its values,
-- the most frequent first, read with its count and keys: the completions of a search of the
-- public catalogues offer them. NULL until the next count.
ALTER TABLE room ADD COLUMN public_values TEXT;
