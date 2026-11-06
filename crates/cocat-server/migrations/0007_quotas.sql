-- Optional storage limits set per account (the rooms it owns) and per room, in MB.
-- NULL means the value configured by COCAT_OWNER_MAX_MB and COCAT_ROOM_MAX_MB.
ALTER TABLE "user" ADD COLUMN quota_mb INTEGER;
ALTER TABLE room ADD COLUMN limit_mb INTEGER;
