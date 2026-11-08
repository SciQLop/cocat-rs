-- Each account's own project, the one made with the account. Clients found it by its name,
-- which a rename loses. Python cocat made it a uuid-hex room named "Personal"; an account
-- imported from Python gets the oldest such room it owns (crate::db::MARK_PERSONAL_ROOMS
-- repeats this for later imports).
ALTER TABLE "user" ADD COLUMN personal_room TEXT REFERENCES room(id) ON DELETE SET NULL;

UPDATE "user" SET personal_room = (
    SELECT room.id
    FROM room JOIN membership ON membership.room_id = room.id
    WHERE membership.user_id = "user".id
      AND membership.role = 'owner'
      AND room.name IN ('My catalogues', 'Personal')
      AND length(room.id) = 32
      AND room.id NOT GLOB '*[^0-9a-f]*'
    ORDER BY room.created_at, room.id
    LIMIT 1
)
WHERE personal_room IS NULL;
