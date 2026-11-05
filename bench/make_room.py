#!/usr/bin/env python3
"""Write a room file with `catalogues × events` map keys, in the Python wire format."""

import argparse
from pathlib import Path

from pycrdt import Doc, Map, create_update_message

HEADER = b"0.0.1\0"


def varuint(value: int) -> bytes:
    """The length prefix the room file uses: 7-bit groups, least significant first."""
    encoded = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        if value:
            encoded.append(byte | 0x80)
        else:
            encoded.append(byte)
            return bytes(encoded)


def catalogue_update(catalogue: int, events: int) -> bytes:
    document = Doc()
    content = document.get("content", type=Map)
    for event in range(events):
        content[f"c-{catalogue}-{event}"] = f"value-{catalogue}-{event}"
    return create_update_message(document.get_update())


def write_room(directory: Path, room: str, catalogues: int, events: int) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / f"{room}.y"
    with path.open("wb") as file:
        file.write(HEADER)
        for catalogue in range(catalogues):
            update = catalogue_update(catalogue, events)
            file.write(varuint(len(update)))
            file.write(update)
    return path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dir", required=True, type=Path)
    parser.add_argument("--room", required=True)
    parser.add_argument("--catalogues", type=int, default=100)
    parser.add_argument("--events", type=int, default=1000)
    arguments = parser.parse_args()

    path = write_room(
        arguments.dir, arguments.room, arguments.catalogues, arguments.events
    )
    print(path.stat().st_size)


if __name__ == "__main__":
    main()
