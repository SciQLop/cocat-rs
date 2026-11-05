"""Build catalogues.y, the document a reader of the catalogue API has to understand.

The file is written the way a room file is written (a version header, then one
length-prefixed update per record), by cocat's own client, so the Rust reader is
checked against the writer it has to stay compatible with. Catalogues alpha and
beta are ordinary; the file also carries two things a live room can contain and a
reader must survive: an event whose start is not a time, and a catalogue listing
an event that does not exist.

Run from the repository root with cocat-python-tests's environment, then commit both
files this script writes:

    /home/jeandet/Documents/prog/cocat-python-tests/.venv/bin/python \
        crates/cocat-core/tests/fixtures/make_catalogues.py
"""

from __future__ import annotations

import datetime
import json
import uuid
from pathlib import Path

from pycrdt import Map

from cocat import DB

HERE = Path(__file__).parent
HEADER = b"0.0.1\0"
UTC = datetime.timezone.utc

ALPHA = uuid.UUID("6f9c1a52-0b0d-4f4e-9d5e-1b2c3d4e5f60")
BETA = uuid.UUID("0e4d2c31-5a6b-4c7d-8e9f-0a1b2c3d4e5f")
MALFORMED = "11111111-2222-4333-8444-555555555555"
DANGLING = "99999999-8888-4777-8666-555555555555"


def varuint(value: int) -> bytes:
    out = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        if value:
            out.append(byte | 0x80)
        else:
            out.append(byte)
            return bytes(out)


def malformed_event() -> Map:
    return Map(
        {
            "uuid": MALFORMED,
            "start": "not a time",
            "stop": "not a time",
            "author": "m@x.org",
            "tags": Map({}),
            "products": Map({}),
            "rating": None,
            "attributes": Map({}),
        }
    )


def build() -> tuple[dict, list[bytes]]:
    database = DB()
    updates: list[bytes] = []
    database.doc.observe(lambda event: updates.append(event.update))

    alpha = database.create_catalogue(
        name="alpha",
        author="a@x.org",
        uuid=ALPHA,
        tags={"t1", "t2"},
        attributes={"mission": "MMS", "level": 2},
    )
    plain = database.create_event(
        start=datetime.datetime(2025, 1, 31, 10, 0, 0),
        stop=datetime.datetime(2025, 1, 31, 10, 1, 0),
        author="a@x.org",
        uuid=uuid.UUID("aaaaaaaa-1111-4111-8111-111111111111"),
        tags={"t1"},
        products={"p1"},
        rating=3,
        attributes={"q": 1.5, "flag": True},
    )
    precise = database.create_event(
        start=datetime.datetime(2025, 1, 31, 11, 0, 0, 123456),
        stop=datetime.datetime(2025, 1, 31, 11, 1, 0, 654321),
        author="a@x.org",
        uuid=uuid.UUID("bbbbbbbb-2222-4222-8222-222222222222"),
        tags=set(),
        products=set(),
        rating=None,
        attributes={"q": 0.5, "flag": False},
    )
    offset = database.create_event(
        start=datetime.datetime(2025, 1, 31, 12, 0, 0, tzinfo=UTC),
        stop=datetime.datetime(2025, 1, 31, 12, 1, 0, tzinfo=UTC),
        author="a@x.org",
        uuid=uuid.UUID("cccccccc-3333-4333-8333-333333333333"),
        tags={"t2"},
        products={"p1", "p2"},
        rating=1,
        attributes={"q": 1.5, "flag": True},
    )
    alpha.add_events([plain, precise, offset])

    beta = database.create_catalogue(
        name="beta", author="b@x.org", uuid=BETA, tags=set(), attributes={}
    )
    with database.transaction():
        database._catalogue_maps[str(BETA)]["events"][DANGLING] = True
        database._event_maps[MALFORMED] = malformed_event()

    expected = {
        "catalogues": [
            {
                "uuid": str(catalogue.uuid),
                "name": catalogue.name,
                "author": catalogue.author,
                "tags": sorted(catalogue.tags),
                "attributes": dict(catalogue.attributes),
            }
            for catalogue in (alpha, beta)
        ],
        "events": [event.to_dict() for event in alpha],
    }
    return expected, updates


def main() -> None:
    expected, updates = build()
    (HERE / "catalogues.y").write_bytes(
        HEADER + b"".join(varuint(len(update)) + update for update in updates)
    )
    (HERE / "catalogues.expected.json").write_text(
        json.dumps(expected, indent=2, sort_keys=True) + "\n"
    )
    print(f"{len(updates)} records, {len(expected['events'])} events")


if __name__ == "__main__":
    main()
