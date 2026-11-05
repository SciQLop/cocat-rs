#!/usr/bin/env python3
"""Write a room file with cocat's own model, not a flat map of strings.

One update per catalogue keeps the file close to what the Python server writes when
it saves a session, while still producing the 100 catalogues x 1000 events room the
spike gate measures.
"""

import argparse
from datetime import datetime, timedelta, timezone
from pathlib import Path

from cocat import DB

HEADER = b"0.0.1\0"
EPOCH = datetime(2020, 1, 1, tzinfo=timezone.utc)


def varuint(value: int) -> bytes:
    encoded = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        if value:
            encoded.append(byte | 0x80)
        else:
            encoded.append(byte)
            return bytes(encoded)


def catalogue_update(database: DB, index: int, events: int) -> bytes:
    state = database.doc.get_state()
    catalogue = database.create_catalogue(name=f"c{index}", author="bench")
    catalogue.add_events(
        [
            database.create_event(
                start=EPOCH + timedelta(minutes=position),
                stop=EPOCH + timedelta(minutes=position + 1),
                author="bench",
                tags=["bench"],
                products=["product"],
                rating=3,
            )
            for position in range(events)
        ]
    )
    return database.doc.get_update(state)


def write_room(directory: Path, room: str, catalogues: int, events: int) -> Path:
    database = DB()
    path = directory / f"{room}.y"
    with path.open("wb") as file:
        file.write(HEADER)
        for index in range(catalogues):
            update = catalogue_update(database, index, events)
            file.write(varuint(len(update)))
            file.write(update)
    return path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dir", type=Path, required=True)
    parser.add_argument("--room", required=True)
    parser.add_argument("--catalogues", type=int, default=100)
    parser.add_argument("--events", type=int, default=1000)
    arguments = parser.parse_args()
    arguments.dir.mkdir(parents=True, exist_ok=True)
    path = write_room(
        arguments.dir, arguments.room, arguments.catalogues, arguments.events
    )
    size = path.stat().st_size
    print(f"{path}: {size} bytes ({size / 1e6:.1f} MB)")


if __name__ == "__main__":
    main()
