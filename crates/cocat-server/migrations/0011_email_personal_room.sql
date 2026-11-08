-- Python production made no "Personal" room: an account's own room is the one whose id is
-- its email's local part (alexis.jeandet for alexis.jeandet@...), owned by that account.
-- crate::db::MARK_EMAIL_ROOMS runs this file again for later imports.
UPDATE "user" SET personal_room = (
    SELECT room.id
    FROM room JOIN membership ON membership.room_id = room.id
    WHERE membership.user_id = "user".id
      AND membership.role = 'owner'
      AND room.id = substr("user".email, 1, instr("user".email, '@') - 1)
)
WHERE personal_room IS NULL;
