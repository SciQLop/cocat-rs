"""Read a room with Python cocat, check what cocat_rs wrote, then write back.

Run by test_cross_client.py with the cocat-python-tests virtualenv: that is the
only interpreter that has Python cocat installed, and it must not be modified.

    python cross_client.py URL PORT ROOM_ID COOKIE EVENT_UUID OUTPUT
"""

import json
import sys
from datetime import datetime

import httpx
from cocat import DB
from pycrdt import Doc
from wire_websocket import WebSocketClient

URL, PORT, ROOM_ID, COOKIE, EVENT, OUTPUT = sys.argv[1:7]
PORT = int(PORT)
PASSWORD = "hunter2"

doc = Doc()
db = DB(doc)
with WebSocketClient(
    id=f"room/{ROOM_ID}",
    doc=doc,
    host="http://127.0.0.1",
    port=PORT,
    cookies={"fastapiusersauth": COOKIE},
) as client:
    client.pull()


    def describe(catalogue, event):
        return {
            "catalogue": {
                "uuid": str(catalogue.uuid),
                "name": catalogue.name,
                "author": catalogue.author,
                "tags": sorted(catalogue.tags),
                "attributes": dict(catalogue.attributes),
            },
            "event": {
                "uuid": str(event.uuid),
                "start": str(event.start),
                "stop": str(event.stop),
                "author": event.author,
                "rating": event.rating,
                "tags": sorted(event.tags),
                "products": sorted(event.products),
                "attributes": dict(event.attributes),
            },
        }


    written = {}
    if db.catalogues:
        written = describe(db.get_catalogue("alpha"), db.get_event(EVENT))

    catalogue = db.create_catalogue(
        name="beta",
        author="python@x.org",
        tags={"p1", "p2"},
        attributes={"mission": "MMS", "level": 3},
    )
    event = db.create_event(
        start=datetime(2025, 2, 1, 9, 0, 0, 123456),
        stop=datetime(2025, 2, 1, 9, 1, 0),
        author="python@x.org",
        tags={"p3"},
        products={"p4"},
        rating=5,
        attributes={"q": 0.25, "flag": False},
    )
    catalogue.add_events([event])
    client.push()

    with open(OUTPUT, "w") as file:
        json.dump(
            {
                "read": written,
                "written": {"catalogue": str(catalogue.uuid), "event": str(event.uuid)},
            },
            file,
        )
