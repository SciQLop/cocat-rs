-- Public catalogues: projects with a "public." id that every signed-in account reads and
-- curators edit. Nobody owns them, so they never get members, group grants or invitations;
-- the triggers keep that true whatever path writes.
ALTER TABLE room ADD COLUMN public BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE room ADD COLUMN source TEXT;
ALTER TABLE room ADD COLUMN folder TEXT;
ALTER TABLE room ADD COLUMN description TEXT;

CREATE TRIGGER no_members_in_public_rooms BEFORE INSERT ON membership
WHEN (SELECT public FROM room WHERE id = NEW.room_id)
BEGIN
    SELECT RAISE(ABORT, 'A public catalogue has no members');
END;

CREATE TRIGGER no_groups_in_public_rooms BEFORE INSERT ON room_group
WHEN (SELECT public FROM room WHERE id = NEW.room_id)
BEGIN
    SELECT RAISE(ABORT, 'A public catalogue is not shared with groups');
END;

CREATE TRIGGER no_invitations_to_public_rooms BEFORE INSERT ON invitation
WHEN (SELECT public FROM room WHERE id = NEW.room_id)
BEGIN
    SELECT RAISE(ABORT, 'A public catalogue takes no invitations');
END;
