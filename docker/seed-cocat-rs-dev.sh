#!/bin/bash
# Replaces the Rust dev server's data with a fresh copy of the Python production data,
# as the sciqlop user. Production keeps running: the SQLite file is copied with SQLite's
# online backup, and a room file copied mid-append ends in a partial record that the
# Rust server drops when it opens the room. Install as /usr/bin/seed-cocat-rs-dev.
set -e
if [[ $EUID -ne 1001 ]]; then
    echo "This script must be run as sciqlop user"
    exit 1
fi
export XDG_RUNTIME_DIR=/run/user/"$(id -u)"

IMAGE=registry.lpp.polytechnique.fr/sciqlop/cocat-server-rs:devel
NAME=cocat-rs-dev
PROD=/DATA/cocat_server
DATA=/DATA/cocat_rs_dev
# Optional: the account that takes the room files no Python user lists.
OWNER=${1:+--owner $1}

if podman container exists "$NAME"; then
    podman stop "$NAME"
fi
# The production files and the dev copy belong to the containers' cocat user (uid 1000
# inside, a subordinate uid outside), so the copy runs inside podman's user namespace.
podman unshare bash -c '
    set -e
    rm -rf "$1"
    mkdir -p "$1/copy" "$1/db" "$1/mail"
    cp -a "$2/storage" "$1/storage"
    python3 -c "import sqlite3, sys; sqlite3.connect(sys.argv[1]).backup(sqlite3.connect(sys.argv[2]))" \
        "$2/db/users.db" "$1/copy/users.db"
    chown -R 1000:1000 "$1"
' seed "$DATA" "$PROD"

podman run --rm --entrypoint /usr/local/bin/cocat-server \
    -v "$DATA/storage:/storage:Z" -v "$DATA/db:/db:Z" -v "$DATA/copy:/copy:Z" \
    "$IMAGE" import --update-dir /storage --db /db/cocat-rs.db \
    --users /copy/users.db $OWNER

if podman container exists "$NAME"; then
    podman start "$NAME"
fi
