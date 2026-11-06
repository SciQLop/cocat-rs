#!/usr/bin/env sh

set -e

echo "Starting cocat server..."
exec cocat-server serve --host "0.0.0.0" --port "$PORT" --update_dir "$STORAGE_PATH" --db_path "$DB_PATH/cocat-rs.db"
