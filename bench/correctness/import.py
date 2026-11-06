#!/usr/bin/env python3
"""Gate: a running Python cocat deployment moves to the Rust server.

The Python server is seeded through its CLI and API, edited through two pycrdt
clients, and stopped. `cocat-server import` then copies its database, and the
Rust server is started on the same room files and the imported database: every
Python account must log in, list the same rooms with the same roles, a fresh
account must accept the invitation Python issued, and every room must hold the
document the Python clients wrote.
"""

import json
import subprocess
import sys
from datetime import UTC, datetime, timedelta
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request, urlopen

from cocat import DB

from common import (
    COOKIE,
    PASSWORD,
    Live,
    Server,
    cli,
    fresh_database,
    login,
    open_socket,
    run,
    state_json,
    workdir,
)

ALICE = "alice@test.org"
BOB = "bob@test.org"
CAROL = "carol@test.org"
INVITEE = "invitee@test.org"
ACCOUNTS = [ALICE, BOB, CAROL]
SHARED = "shared-room"
SOLO = "solo-room"
ROOMS = [SHARED, SOLO]
EPOCH = datetime(2020, 1, 1, tzinfo=UTC)


def call(
    server: Server,
    method: str,
    path: str,
    cookie: str | None = None,
    body: object = None,
) -> tuple[int, object]:
    """One JSON call; a refusal is a status and a body, not an exception."""
    data = None if body is None else json.dumps(body).encode()
    request = Request(server.base_url + path, data=data, method=method)
    if data is not None:
        request.add_header("content-type", "application/json")
    if cookie is not None:
        request.add_header("cookie", f"{COOKIE}={cookie}")
    try:
        with urlopen(request, timeout=10) as response:
            raw = response.read()
            return response.status, json.loads(raw) if raw else None
    except HTTPError as error:
        raw = error.read()
        return error.code, json.loads(raw) if raw else None


def room_list(server: Server, cookie: str) -> dict[str, tuple[str, str]]:
    status, rooms = call(server, "GET", "/api/rooms", cookie)
    if status != 200:
        raise SystemExit(f"GET /api/rooms answered {status}: {rooms!r}")
    return {room["id"]: (room["name"], room["role"]) for room in rooms}


def seed_python(db_path: Path) -> None:
    for email in ACCOUNTS:
        run(
            cli("python"),
            "create-user",
            "--email",
            email,
            "--password",
            PASSWORD,
            "--db_path",
            db_path,
        )
    for room in ROOMS:
        run(
            cli("python"),
            "create-room",
            "--owner",
            ALICE,
            "--room-id",
            room,
            "--name",
            f"{room} name",
            "--db_path",
            db_path,
        )
    run(
        cli("python"),
        "add-user-to-room",
        "--email",
        BOB,
        "--room-id",
        SHARED,
        "--db_path",
        db_path,
    )
    run(
        cli("python"),
        "add-user-to-room",
        "--email",
        CAROL,
        "--room-id",
        SHARED,
        "--db_path",
        db_path,
    )


def demote_to_viewer(server: Server, cookie: str, room: str, email: str) -> None:
    status, members = call(server, "GET", f"/api/rooms/{room}/members", cookie)
    if status != 200:
        raise SystemExit(f"GET the members of {room} answered {status}: {members!r}")
    member = next(member for member in members if member["email"] == email)
    status, body = call(
        server,
        "PATCH",
        f"/api/rooms/{room}/members/{member['user_id']}",
        cookie,
        {"role": "viewer"},
    )
    if status != 204:
        raise SystemExit(f"PATCH the role of {email} answered {status}: {body!r}")


def edit(database: DB, room: str) -> None:
    """Both root maps, a rename, a delete: enough that a lost update would show."""
    catalogue = database.create_catalogue(
        name=f"{room} catalogue", author="gate", tags=[room]
    )
    events = [
        database.create_event(
            start=EPOCH + timedelta(minutes=index),
            stop=EPOCH + timedelta(minutes=index + 1),
            author="gate",
            tags=["gate", f"n{index}"],
            products=["product"],
            rating=index % 5,
        )
        for index in range(4)
    ]
    catalogue.add_events(events[:3])
    events[0].rating = 4
    events[1].tags = {"edited"}
    events[2].delete()


def write_room(server: Server, cookie: str, room: str) -> str:
    """Edit one room through two clients; return the state the server broadcast."""
    writer_socket = open_socket(server, room, cookie)
    reader_socket = open_socket(server, room, cookie)
    writer = Live(writer_socket, fresh_database(writer_socket))
    reader = Live(reader_socket, fresh_database(reader_socket))
    edit(writer.database, room)
    reader.sync_to(writer.database.doc)
    served = state_json(reader.database.doc)
    if served != state_json(writer.database.doc):
        raise SystemExit(f"the clients of {room} disagree before the import")
    writer.close()
    reader.close()
    return served


def import_deployment(update_dir: Path, rust_db: Path, python_db: Path) -> list[str]:
    result = subprocess.run(
        [
            str(cli("rust")),
            "import",
            "--update-dir",
            str(update_dir),
            "--db",
            str(rust_db),
            "--users",
            str(python_db),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    lines = [line for line in result.stdout.splitlines() if line.strip()]
    for line in lines:
        print(f"import: {line}")
    for line in result.stderr.splitlines():
        print(f"import: {line}")
    return lines


def expect_import_summary(
    lines: list[str], before: dict[str, dict[str, tuple[str, str]]]
) -> None:
    """The counts the Python server reported: every room each account could see."""
    seen = {room for listing in before.values() for room in listing}
    memberships = sum(len(listing) for listing in before.values())
    wanted = (
        f"imported {len(ACCOUNTS)} users, {len(seen)} rooms, {memberships} memberships,"
        " 1 invitations, 0 linked accounts"
    )
    if not lines or lines[0] != wanted:
        raise SystemExit(f"the import summary is {lines[0]!r}, not {wanted!r}")


def check_logins(server: Server, before: dict[str, dict[str, tuple[str, str]]]):
    """Every Python account logs in on Rust and sees the rooms Python showed it."""
    after = {}
    for email in ACCOUNTS:
        cookie = login(server, email)
        after[email] = room_list(server, cookie)
        if after[email] != before[email]:
            difference(email, before[email], after[email])
            raise SystemExit(f"{email} sees different rooms after the import")
    print(f"rust: {', '.join(ACCOUNTS)} log in and see the Python rooms")
    return after


def difference(email: str, before: dict, after: dict) -> None:
    for room in sorted(set(before) | set(after)):
        if before.get(room) != after.get(room):
            print(f"  {email} {room}: python {before.get(room)}")
            print(f"  {email} {room}: rust   {after.get(room)}")


def check_invitation(server: Server, token: str) -> None:
    cookie = login(server, INVITEE)
    status, room = call(
        server, "POST", "/api/invitations/accept", cookie, {"token": token}
    )
    if status != 200:
        raise SystemExit(f"accepting the Python invitation answered {status}: {room!r}")
    if room["id"] != SHARED or room["role"] != "editor":
        raise SystemExit(f"the accepted invitation returned {room!r}")
    joined = room_list(server, cookie)
    if joined.get(SHARED) is None or joined[SHARED][1] != "editor":
        raise SystemExit(
            f"{INVITEE} does not hold the room after accepting: {joined!r}"
        )
    print(f"rust: {INVITEE} accepts the Python invitation into {SHARED} as editor")


def check_documents(server: Server, cookie: str, served: dict[str, str]) -> None:
    for room, wanted in served.items():
        socket = open_socket(server, room, cookie)
        document = fresh_database(socket)
        socket.close()
        if state_json(document.doc) != wanted:
            raise SystemExit(f"{room} does not hold the Python client's document")
    print(f"rust: {len(served)} rooms hold the Python client's document")


def main() -> int:
    directory = workdir("import", "e2e")
    updates = directory / "updates"
    python_db = directory / "python.db"
    rust_db = directory / "rust.db"

    seed_python(python_db)
    served: dict[str, str] = {}
    before: dict[str, dict[str, tuple[str, str]]] = {}
    with Server("python", updates, python_db) as server:
        owner = login(server, ALICE)
        demote_to_viewer(server, owner, SHARED, CAROL)
        for room in ROOMS:
            served[room] = write_room(server, owner, room)
        status, invitation = call(
            server,
            "POST",
            f"/api/rooms/{SHARED}/invitations",
            owner,
            {"role": "editor", "expires_in_days": 14},
        )
        if status != 201:
            raise SystemExit(
                f"creating the invitation answered {status}: {invitation!r}"
            )
        before = {email: room_list(server, login(server, email)) for email in ACCOUNTS}
    print(f"python: {len(ACCOUNTS)} accounts, {len(ROOMS)} rooms, 1 invitation")
    for room in ROOMS:
        print(f"python: {room} {len(served[room])} bytes of json")

    lines = import_deployment(updates, rust_db, python_db)
    expect_import_summary(lines, before)

    run(
        cli("rust"),
        "create-user",
        "--email",
        INVITEE,
        "--password",
        PASSWORD,
        "--db_path",
        rust_db,
    )
    with Server("rust", updates, rust_db) as server:
        check_logins(server, before)
        check_invitation(server, invitation["token"])
        check_documents(server, login(server, ALICE), served)

    print("rust: PASS the Python deployment runs on the Rust server")
    return 0


if __name__ == "__main__":
    sys.exit(main())
