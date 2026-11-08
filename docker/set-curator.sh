#!/usr/bin/env sh

set -e
if [ "$2" = "--revoke" ]; then
    cocat-server set-curator --email "$1" --revoke --db_path "$DB_PATH/cocat-rs.db"
else
    cocat-server set-curator --email "$1" --db_path "$DB_PATH/cocat-rs.db"
fi
