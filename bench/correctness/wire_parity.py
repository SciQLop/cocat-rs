#!/usr/bin/env python3
"""Gate: the two servers answer the same sync bytes for the same room.

Both servers open a byte-identical copy of one room file and receive the identical
SyncStep1 (an empty state vector) and the identical update. The gate compares the
SyncStep2 bytes and the bytes the update produces. When the bytes differ it decodes
both answers and compares their states, so a difference caused by encoding rather
than content is reported as such, not as a protocol bug.
"""

import hashlib
import json
import shutil
import sys
from datetime import UTC, datetime

from cocat import DB
from pycrdt import (
    Doc,
    YMessageType,
    YSyncMessageType,
    create_sync_message,
    create_update_message,
    handle_sync_message,
)

from common import (
    SYNC_TIMEOUT,
    RoomFile,
    Server,
    login,
    open_socket,
    seed,
    state_json,
    workdir,
)

ROOM = "gates"
EPOCH = datetime(2020, 1, 1, tzinfo=UTC)
SERVERS = ["python", "rust"]


def fixture(path) -> None:
    """One room, written once, then copied so both servers start from equal bytes."""
    room = RoomFile()
    database = room.database
    for index in range(2):
        catalogue = database.create_catalogue(name=f"c{index}", author="gate")
        catalogue.add_events(
            [
                database.create_event(
                    start=EPOCH.replace(minute=position),
                    stop=EPOCH.replace(minute=position + 1),
                    author="gate",
                    tags=["gate"],
                    rating=position % 5,
                )
                for position in range(5)
            ]
        )
    room.write(path)


def one_update() -> bytes:
    """The update every server will receive, byte for byte."""
    sent: list[bytes] = []
    database = DB()
    database.doc.observe(lambda event: sent.append(event.update))
    database.create_event(
        start=EPOCH.replace(day=2),
        stop=EPOCH.replace(day=3),
        author="gate",
        tags=["pushed"],
    )
    return sent[0]


def next_frame(socket, kind: int, seen: list[bytes]) -> bytes:
    """The next sync frame of `kind`; everything else is collected in `seen`."""
    while True:
        frame = socket.recv(timeout=SYNC_TIMEOUT)
        seen.append(frame)
        if frame[0] == YMessageType.SYNC and frame[1] == kind:
            return frame


def answer(name: str, room_file, update: bytes) -> dict[str, bytes | str]:
    """What one server answers to the identical step1 and update."""
    directory = workdir("wire_parity", name)
    db_path = directory / "gate.db"
    seed(db_path, name, [ROOM])
    shutil.copy(room_file, directory / "updates" / f"{ROOM}.y")

    with Server(name, directory / "updates", db_path) as server:
        cookie = login(server)
        socket = open_socket(server, ROOM, cookie)
        seen: list[bytes] = []
        socket.send(create_sync_message(Doc()))
        step2 = next_frame(socket, YSyncMessageType.SYNC_STEP2, seen)
        socket.send(create_update_message(update))
        broadcast = next_frame(socket, YSyncMessageType.SYNC_UPDATE, seen)
        socket.close()

    return {
        "step2": step2,
        "broadcast": broadcast,
        "state": state_json(decode(step2)),
        "after_update": state_json(decode(broadcast)),
    }


def decode(frame: bytes) -> Doc:
    document = Doc()
    handle_sync_message(frame[1:], document)
    return document


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()[:16]


def main() -> int:
    room_file = workdir("wire_parity_fixture", "shared") / f"{ROOM}.y"
    fixture(room_file)
    update = one_update()
    print(
        f"room {room_file.stat().st_size} bytes, step1 {len(create_sync_message(Doc()))} bytes, update {len(update)} bytes"
    )

    answers = {name: answer(name, room_file, update) for name in SERVERS}

    for name in SERVERS:
        result = answers[name]
        print(
            f"{name}: step2 {len(result['step2'])} bytes sha256 {digest(result['step2'])}, "
            f"broadcast {len(result['broadcast'])} bytes sha256 {digest(result['broadcast'])}"
        )

    identical = True
    for key in ("step2", "broadcast"):
        left, right = answers["python"][key], answers["rust"][key]
        same = left == right
        identical &= same
        print(f"{key}: {'identical bytes' if same else 'different bytes'}")
        if not same:
            first = next(
                (index for index, (a, b) in enumerate(zip(left, right)) if a != b),
                min(len(left), len(right)),
            )
            print(f"  first difference at byte {first}")

    same_state = answers["python"]["state"] == answers["rust"]["state"]
    same_after = answers["python"]["after_update"] == answers["rust"]["after_update"]
    print(f"decoded state after step2: {'equal' if same_state else 'DIFFERENT'}")
    print(f"decoded state after the update: {'equal' if same_after else 'DIFFERENT'}")

    if identical:
        print("PASS the servers speak identical bytes")
    elif same_state and same_after:
        print(
            "PASS the servers agree on content; the bytes differ because each encodes "
            "the same v1 update with its own encoder (yrs in Rust, lib0 in pycrdt)"
        )
    else:
        print("FAIL the servers disagree on content")
        return 1

    if not same_state:
        diff(answers["python"]["state"], answers["rust"]["state"])
        return 1
    return 0


def diff(left: str, right: str) -> None:
    a, b = json.loads(left), json.loads(right)
    for key in sorted(set(a) | set(b)):
        if a.get(key) != b.get(key):
            print(f"  root key {key!r} differs")


if __name__ == "__main__":
    sys.exit(main())
