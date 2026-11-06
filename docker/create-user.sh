#!/usr/bin/env sh

set -e
cocat-server create-user --email "$1" --password "$2" --db_path "$DB_PATH/cocat-rs.db"
