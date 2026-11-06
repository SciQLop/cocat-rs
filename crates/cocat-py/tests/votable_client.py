"""Exchange a VOTable with Python cocat, in both directions.

Run by test_votable.py with the cocat-python-tests virtualenv, which is the only
interpreter that has Python cocat installed; it must not be modified.

    python votable_client.py export PATH JSON
    python votable_client.py import PATH JSON
"""

import datetime
import json
import sys
import uuid

from cocat import DB
from cocat.votable import export_votable_file, import_votable_file

MODE, PATH, OUTPUT = sys.argv[1:4]

ALPHA = uuid.UUID("6f9c1a52-0b0d-4f4e-9d5e-1b2c3d4e5f60")
FIRST = uuid.UUID("aaaaaaaa-1111-4111-8111-111111111111")
SECOND = uuid.UUID("bbbbbbbb-2222-4222-8222-222222222222")


def describe(event) -> dict:
    return {
        "uuid": str(event.uuid),
        "start": str(event.start),
        "stop": str(event.stop),
        "author": event.author,
        "rating": event.rating,
        "tags": sorted(event.tags),
        "products": sorted(event.products),
        "attributes": dict(event.attributes),
    }


def export() -> dict:
    database = DB()
    catalogue = database.create_catalogue(
        name="gamma",
        author="python@x.org",
        uuid=ALPHA,
        tags={"t1"},
        attributes={"mission": "MMS"},
    )
    first = database.create_event(
        start=datetime.datetime(2025, 1, 31, 10, 0, 0, tzinfo=datetime.timezone.utc),
        stop=datetime.datetime(2025, 1, 31, 10, 1, 0, tzinfo=datetime.timezone.utc),
        author="python@x.org",
        uuid=FIRST,
        tags={"p3"},
        products={"p4"},
        rating=5,
        attributes={"q": 0.25, "flag": False, "mission": "MMS"},
    )
    second = database.create_event(
        start=datetime.datetime(2025, 1, 31, 11, 0, 0, 123456),
        stop=datetime.datetime(2025, 1, 31, 11, 1, 0, 654321),
        author="python@x.org",
        uuid=SECOND,
        tags=set(),
        products=set(),
        rating=None,
        attributes={"q": 1.5, "flag": True, "mission": "MMS"},
    )
    catalogue.add_events([first, second])
    export_votable_file(catalogue, PATH)
    return {"events": [describe(first), describe(second)]}


def read() -> dict:
    database = DB()
    import_votable_file(PATH, database)
    return {
        "catalogues": [
            {
                "name": catalogue.name,
                "author": catalogue.author,
            }
            for catalogue in database.catalogues
        ],
        "events": [describe(event) for event in database.events],
    }


if MODE == "export":
    result = export()
else:
    result = read()

with open(OUTPUT, "w") as file:
    json.dump(result, file, default=str)
