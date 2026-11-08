-- Whether the project's editors may share it (invite, change roles, remove members, grant
-- groups) as its owner does. On by default, for existing projects too, as in Google Docs.
ALTER TABLE room ADD COLUMN editors_can_share BOOLEAN NOT NULL DEFAULT TRUE;
