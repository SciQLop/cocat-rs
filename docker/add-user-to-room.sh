#!/usr/bin/env sh

set -e
cocat-server add-user-to-room --email "$1" --room_id "$2" --db_path "$DB_PATH/cocat-rs.db"
