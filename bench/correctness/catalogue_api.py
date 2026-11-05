#!/usr/bin/env python3
"""Gate: the read-only catalogue API answers what the client's document holds.

A pycrdt client fills a room through `cocat.DB`, then plain HTTP reads it back: the
`/events` JSON must match the client's own view event by event, and the `.votable`
answer, imported into a fresh `DB` by `cocat.import_votable_str`, must give the same
events again. The room is read while it is open and once more after the client left,
so both the live room and the cold file path are exercised.
"""

import json
import sys
from datetime import UTC, datetime, timedelta
from urllib.request import Request, urlopen

from cocat import DB
from cocat.votable import import_votable_str
from common import (
    COOKIE,
    Server,
    fresh_database,
    login,
    open_socket,
    publish,
    seed,
    server_name,
    workdir,
)

ROOM = "catalogue-api"
CATALOGUE = "gates"
MARKUP = "tag<&>"
EPOCH = datetime(2020, 1, 1, tzinfo=UTC)
EVENTS = 50


def populate(database: DB) -> None:
    """Two catalogues, fifty events: null and real ratings, an int attribute, markup."""
    catalogue = database.create_catalogue(
        name=CATALOGUE,
        author="gate",
        tags=[MARKUP, "gate"],
        attributes={"mission": "MMS"},
    )
    spare = database.create_catalogue(name="spare", author="gate")
    events = [
        database.create_event(
            start=EPOCH + timedelta(minutes=index),
            stop=EPOCH + timedelta(minutes=index + 1),
            author="gate",
            tags=[MARKUP, f"n{index % 3}"],
            products=["product"],
            rating=None if index % 4 == 0 else index % 5,
            attributes={"level": 2 + index % 3, "note": "a<b & c"},
        )
        for index in range(EVENTS)
    ]
    catalogue.add_events(events[:40])
    spare.add_events(events[40:])


def get(server: Server, cookie: str, path: str) -> tuple[int, dict[str, str], bytes]:
    request = Request(server.base_url + path, headers={"cookie": f"{COOKIE}={cookie}"})
    with urlopen(request, timeout=60) as response:
        return response.status, dict(response.headers), response.read()


def fetch(server: Server, cookie: str, room: str, catalogue: str) -> list[dict]:
    status, _, body = get(
        server, cookie, f"/api/rooms/{room}/catalogues/{catalogue}/events?limit=1000"
    )
    if status != 200:
        raise SystemExit(f"/events answered {status}: {body[:200]!r}")
    return [normalise(event) for event in json.loads(body)["events"]]


def fetch_votable(server: Server, cookie: str, room: str, catalogue: str) -> DB:
    status, headers, body = get(
        server, cookie, f"/api/rooms/{room}/catalogues/{catalogue}.votable"
    )
    if status != 200:
        raise SystemExit(f"the .votable answer was {status}: {body[:200]!r}")
    print(f"content-type {headers.get('content-type')}")
    print(f"content-disposition {headers.get('content-disposition')}")
    xml = body.decode()
    print(f"votable {len(xml)} bytes, level column {'name="level"' in xml}")
    imported = DB()
    import_votable_str(xml, imported)
    return imported


def expected(database: DB, catalogue: str) -> list[dict]:
    events = database.get_catalogue(catalogue).to_dict()["events"]
    return sorted((normalise(event) for event in events), key=order)


def order(event: dict) -> tuple[str, str]:
    return event["start"], event["uuid"]


def normalise(event: dict) -> dict:
    return {
        "uuid": str(event["uuid"]),
        "start": instant(event["start"]),
        "stop": instant(event["stop"]),
        "author": event["author"],
        "tags": sorted(event["tags"]),
        "products": sorted(event["products"]),
        "rating": event["rating"],
        "attributes": event["attributes"],
    }


def instant(value: object) -> str:
    if isinstance(value, datetime):
        moment = value
    else:
        moment = datetime.fromisoformat(str(value))
    if moment.tzinfo is None:
        moment = moment.replace(tzinfo=UTC)
    return moment.astimezone(UTC).isoformat()


def difference(left: list[dict], right: list[dict]) -> None:
    for index, (mine, theirs) in enumerate(zip(left, right, strict=False)):
        if mine != theirs:
            for key in mine:
                if mine[key] != theirs[key]:
                    print(f"  event {index} {key}: expected {mine[key]!r}")
                    print(f"  event {index} {key}: answered {theirs[key]!r}")
            return
    if len(left) != len(right):
        print(f"  {len(left)} events expected, {len(right)} answered")


def main() -> int:
    name = server_name()
    if name != "rust":
        raise SystemExit("the catalogue API exists only in the Rust server")
    directory = workdir("catalogue_api", name)
    db_path = directory / "gate.db"
    seed(db_path, name, [ROOM])

    with Server(name, directory / "updates", db_path) as server:
        cookie = login(server)
        socket = open_socket(server, ROOM, cookie)
        database = fresh_database(socket)
        publish(socket, database)
        populate(database)

        verifier_socket = open_socket(server, ROOM, cookie)
        fresh_database(verifier_socket)
        catalogue = database.get_catalogue(CATALOGUE).to_dict()["uuid"]
        wanted = expected(database, catalogue)
        print(f"client holds {len(wanted)} events, catalogue {catalogue}")

        answered = fetch(server, cookie, ROOM, catalogue)
        print(f"the open room answers {len(answered)} events")
        if answered != wanted:
            difference(wanted, answered)
            print(f"{name}: FAIL the open room's events differ from the client's")
            return 1

        imported = fetch_votable(server, cookie, ROOM, catalogue)
        round_trip = expected(imported, CATALOGUE)
        ratings = {rating(event["rating"]) for event in round_trip}
        wanted_ratings = {rating(event["rating"]) for event in wanted}
        print(
            f"votable round trip: {len(round_trip)} events,"
            f" ratings {sorted(ratings, key=str)}"
        )
        if ratings != wanted_ratings:
            print(
                f"{name}: FAIL the rating did not survive the VOTable round trip"
                f" ({sorted(wanted_ratings, key=str)} became {sorted(ratings, key=str)})"
            )
            return 1
        if not same_events(wanted, round_trip):
            difference(wanted, round_trip)
            print(f"{name}: FAIL the VOTable answer is not the client's document")
            return 1

        verifier_socket.close()
        socket.close()

    # The client is gone: the next read loads the room from its file and drops it again.
    with Server(name, directory / "updates", db_path) as server:
        cookie = login(server)
        cold = fetch(server, cookie, ROOM, catalogue)
        print(f"the closed room answers {len(cold)} events from its file")
        if cold != wanted:
            difference(wanted, cold)
            print(f"{name}: FAIL the cold read differs from the client's")
            return 1

    print(f"{name}: PASS the catalogue API answers the client's document")
    return 0


def rating(value: object) -> object:
    """Python's own VOTable mapping: a null rating and a 0 share the `long` column."""
    return None if value in (0, None) else value


def same_events(left: list[dict], right: list[dict]) -> bool:
    return len(left) == len(right) and all(
        {
            **mine,
            "rating": rating(mine["rating"]),
        }
        == {
            **theirs,
            "rating": rating(theirs["rating"]),
        }
        for mine, theirs in zip(left, right, strict=True)
    )


if __name__ == "__main__":
    sys.exit(main())
