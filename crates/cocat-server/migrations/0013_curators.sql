-- Curators look after the public catalogues. Administrators are curators without the flag.
ALTER TABLE "user" ADD COLUMN is_curator BOOLEAN NOT NULL DEFAULT FALSE;
