#!/usr/bin/env python3
"""Gate: the room a server wrote reopens to the client's own document.

A pycrdt client edits a room through `cocat.DB`, the server is stopped, and the
room's `.y` is then opened by pycrdt alone (no server, no Rust code): the two root
maps must be the same JSON. That is the "file parity" row of the design's gate
table, in the direction the design words it.
"""

import hashlib
import json
import sys
from datetime import UTC, datetime, timedelta

from cocat import DB

from common import (
    Server,
    fresh_database,
    login,
    open_socket,
    publish,
    read_room,
    seed,
    server_name,
    state_json,
    workdir,
)

ROOM = "gates"
EPOCH = datetime(2020, 1, 1, tzinfo=UTC)


def edits(database: DB) -> None:
    """The edits a client makes: the model's whole surface, not just new events."""
    catalogues = [
        database.create_catalogue(name=name, author="gate", tags=[name])
        for name in ("alpha", "beta")
    ]
    events = [
        database.create_event(
            start=EPOCH + timedelta(minutes=index),
            stop=EPOCH + timedelta(minutes=index + 1),
            author="gate",
            tags=["gate", f"n{index}"],
            products=["product"],
            rating=index % 5,
        )
        for index in range(12)
    ]
    catalogues[0].add_events(events[:6])
    catalogues[1].add_events(events[6:])
    events[0].rating = 4
    events[0].tags = {"edited"}
    events[1].start = EPOCH + timedelta(days=1)
    catalogues[1].name = "renamed"
    catalogues[0].remove_events(events[0])
    events[2].delete()


def main() -> int:
    name = server_name()
    directory = workdir("file_parity", name)
    db_path = directory / "gate.db"
    seed(db_path, name, [ROOM])
    updates = directory / "updates"

    with Server(name, updates, db_path) as server:
        cookie = login(server)
        socket = open_socket(server, ROOM, cookie)
        database = fresh_database(socket)
        publish(socket, database)
        edits(database)

        verifier_socket = open_socket(server, ROOM, cookie)
        verifier = fresh_database(verifier_socket)
        served = state_json(verifier.doc)
        print(
            f"{name}: the server holds the client's state: {served == state_json(database.doc)}"
        )
        verifier_socket.close()
        socket.close()

    path = updates / f"{ROOM}.y"
    document = read_room(path)
    reopened = DB(doc=document).to_dict()
    live = state_json(database.doc)
    stored = state_json(document)

    print(f"{name}: {path.stat().st_size} bytes ({path.stat().st_size / 1e3:.1f} kB)")
    print(f"{name}: client  {len(live)} bytes of json, sha256 {hash_of(live)}")
    print(f"{name}: reopened {len(stored)} bytes of json, sha256 {hash_of(stored)}")
    print(
        f"{name}: catalogues {len(reopened['catalogues'])}, events {len(reopened['events'])}"
    )

    if served != live:
        print(f"{name}: FAIL the server never held the client's state")
        return 1
    if live != stored:
        difference(live, stored)
        print(f"{name}: FAIL the reopened room is not the client's document")
        return 1
    print(f"{name}: PASS the file reopens to the client's document")
    return 0


def hash_of(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()[:16]


def difference(live: str, stored: str) -> None:
    """Print the first place the two JSON documents disagree."""
    left = json.loads(live)
    right = json.loads(stored)
    for key in sorted(set(left) | set(right)):
        if left.get(key) != right.get(key):
            print(f"  root key {key!r} differs")
            for ident in sorted(set(left.get(key, {})) | set(right.get(key, {}))):
                if left.get(key, {}).get(ident) != right.get(key, {}).get(ident):
                    print(f"    {ident}: client {left[key].get(ident)}")
                    print(f"    {ident}: stored {right[key].get(ident)}")
                    return


if __name__ == "__main__":
    sys.exit(main())
