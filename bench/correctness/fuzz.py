#!/usr/bin/env python3
"""Gate: two clients fuzzing one room converge, and both servers end up equal.

The op stream is a function of the seed and of the state both clients share, so the
Python and the Rust run replay the same edits: after the last op the two clients
must hold the same document, and that document's hash must be the one the other
server produced. Run it once per server; the second run compares the two.
"""

import hashlib
import json
import os
import random
import sys
from datetime import UTC, datetime, timedelta
from pathlib import Path
from uuid import UUID

from cocat import DB, Catalogue, Event

from common import (
    WORK,
    Live,
    Server,
    fresh_database,
    login,
    open_socket,
    seed,
    server_name,
    state_json,
    workdir,
)

ROOM = "gates"
SEED = 20261105
OPS = int(os.environ.get("FUZZ_OPS", "10000"))
EPOCH = datetime(2020, 1, 1, tzinfo=UTC)
TAGS = ["gate", "alpha", "beta", "gamma", "delta"]
PRODUCTS = ["product", "other", "third"]
AUTHORS = ["ada", "grace", "alan", "edsger"]
MAX_EVENTS = 150
MAX_CATALOGUES = 25


def identifier(kind: str, tick: int) -> UUID:
    """A uuid both runs compute from the same tick, so the two states can be equal."""
    return UUID(int=(0xC0CA7 << 40) + tick * 2 + (kind == "catalogue"))


def when(minutes: int) -> datetime:
    return EPOCH + timedelta(minutes=minutes)


def create_event(database: DB, rng: random.Random, tick: int) -> None:
    database.create_event(
        uuid=identifier("event", tick),
        start=when(rng.randrange(0, 5_000_000)),
        stop=when(rng.randrange(5_000_001, 10_000_000)),
        author=rng.choice(AUTHORS),
        tags=rng.sample(TAGS, rng.randrange(0, 3)),
        products=rng.sample(PRODUCTS, rng.randrange(0, 2)),
        rating=rng.randrange(0, 6),
    )


def create_catalogue(database: DB, rng: random.Random, tick: int) -> None:
    database.create_catalogue(
        uuid=identifier("catalogue", tick),
        name=f"catalogue-{tick % 7}",
        author=rng.choice(AUTHORS),
        tags=rng.sample(TAGS, rng.randrange(0, 3)),
    )


def pick(pool: list, rng: random.Random):
    return rng.choice(pool)


def delete_event(database: DB, rng: random.Random) -> None:
    pick(sorted(database.events, key=lambda event: event.uuid), rng).delete()


def delete_catalogue(database: DB, rng: random.Random) -> None:
    pick(sorted(database.catalogues, key=lambda item: item.uuid), rng).delete()


def add_events(database: DB, rng: random.Random) -> None:
    catalogue: Catalogue = pick(
        sorted(database.catalogues, key=lambda item: item.uuid), rng
    )
    events = sorted(database.events, key=lambda event: event.uuid)
    catalogue.add_events(rng.sample(events, rng.randrange(1, min(4, len(events)) + 1)))


def remove_events(database: DB, rng: random.Random) -> None:
    with_events = [
        catalogue
        for catalogue in sorted(database.catalogues, key=lambda item: item.uuid)
        if catalogue.events
    ]
    if not with_events:
        return create_event(database, rng, rng.randrange(10_000, 20_000))
    catalogue = pick(with_events, rng)
    events = sorted(catalogue.events, key=lambda event: event.uuid)
    catalogue.remove_events(
        rng.sample(events, rng.randrange(1, min(3, len(events)) + 1))
    )


def set_event_field(database: DB, rng: random.Random) -> None:
    event: Event = pick(sorted(database.events, key=lambda item: item.uuid), rng)
    match rng.randrange(0, 4):
        case 0:
            event.rating = rng.randrange(0, 6)
        case 1:
            event.start = when(rng.randrange(0, 5_000_000))
        case 2:
            event.tags = set(rng.sample(TAGS, rng.randrange(0, 3)))
        case 3:
            event.products = set(rng.sample(PRODUCTS, rng.randrange(0, 2)))


def set_catalogue_field(database: DB, rng: random.Random) -> None:
    catalogue: Catalogue = pick(
        sorted(database.catalogues, key=lambda item: item.uuid), rng
    )
    match rng.randrange(0, 2):
        case 0:
            catalogue.name = f"renamed-{rng.randrange(0, 9)}"
        case 1:
            catalogue.tags = set(rng.sample(TAGS, rng.randrange(0, 3)))


def apply(database: DB, rng: random.Random, tick: int) -> str:
    """One op, chosen from the state so both runs take the same branch."""
    events, catalogues = len(database.events), len(database.catalogues)
    if events >= MAX_EVENTS:
        delete_event(database, rng)
        return "delete_event"
    if catalogues >= MAX_CATALOGUES:
        delete_catalogue(database, rng)
        return "delete_catalogue"
    choices = [
        (create_event, 5),
        (create_catalogue, 3),
        (lambda db, r: delete_event(db, r), 2 if events else 0),
        (lambda db, r: delete_catalogue(db, r), 1 if catalogues else 0),
        (add_events, 4 if events and catalogues else 0),
        (remove_events, 2 if catalogues else 0),
        (set_event_field, 3 if events else 0),
        (set_catalogue_field, 2 if catalogues else 0),
    ]
    weighted = [(op, weight) for op, weight in choices if weight]
    operations, weights = zip(*weighted)
    operation = rng.choices(operations, weights)[0]
    if operation in (create_event, create_catalogue):
        operation(database, rng, tick)
        return operation.__name__
    operation(database, rng)
    return getattr(operation, "__name__", "adapted")


def hash_of(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()


def main() -> int:
    name = server_name()
    directory = workdir("fuzz", name)
    seed(directory / "gate.db", name, [ROOM])
    rng = random.Random(SEED)
    updates = 0

    with Server(name, directory / "updates", directory / "gate.db") as server:
        cookie = login(server)
        clients = []
        for _ in range(2):
            socket = open_socket(server, ROOM, cookie)
            database = fresh_database(socket)
            clients.append(Live(socket, database))

        first, second = clients
        for tick in range(OPS):
            actor, other = (first, second) if tick % 2 == 0 else (second, first)
            apply(actor.database, rng, tick)
            frames = other.sync_to(actor.database.doc)
            actor.pull(frames)
            updates += frames
            if state_json(actor.database.doc) != state_json(other.database.doc):
                print(f"{name}: FAIL clients disagree after op {tick}")
                return 1
            if tick % 1000 == 0:
                print(f"{name}: {tick} ops, {len(actor.database.events)} events")

        for client in clients:
            client.close()

    published = first.sent + second.sent
    print(f"{name}: {published} local updates, {updates} broadcasts read")
    state = state_json(first.database.doc)
    digest = hash_of(state)
    events = len(first.database.events)
    catalogues = len(first.database.catalogues)
    print(
        f"{name}: {OPS} ops, {updates} updates, {events} events, "
        f"{catalogues} catalogues, sha256 {digest}"
    )
    WORK.mkdir(parents=True, exist_ok=True)
    Path(WORK / f"fuzz-{name}.json").write_text(
        json.dumps(
            {
                "seed": SEED,
                "ops": OPS,
                "updates": updates,
                "events": events,
                "catalogues": catalogues,
                "sha256": digest,
                "state": state,
            }
        )
    )
    return compare(name, digest)


def compare(name: str, digest: str) -> int:
    other = "python" if name == "rust" else "rust"
    path = Path(WORK / f"fuzz-{other}.json")
    if not path.exists():
        print(
            f"{name}: PASS the clients converge; run SERVER={other} to compare the servers"
        )
        return 0
    saved = json.loads(path.read_text())
    if saved["seed"] != SEED or saved["ops"] != OPS:
        print(
            f"{name}: cannot compare with {other}, it ran seed {saved['seed']} over {saved['ops']} ops"
        )
        return 1
    if saved["sha256"] != digest:
        print(f"{name}: FAIL {name} {digest[:16]} != {other} {saved['sha256'][:16]}")
        return 1
    print(f"{name}: PASS both servers end on {digest[:16]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
