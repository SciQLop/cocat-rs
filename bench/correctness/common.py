"""Helpers shared by the phase 0 correctness gates.

Every gate starts a server from `SERVER=rust|python` with the same room files and a
database seeded by that server's own CLI, talks to it with pycrdt (the client stack
cocat-python-tests uses), and kills the server it started.
"""

import json
import os
import signal
import socket
import subprocess
import time
from pathlib import Path
from typing import Self
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import Request, urlopen

from cocat import DB
from pycrdt import (
    Doc,
    Map,
    YMessageType,
    YSyncMessageType,
    create_sync_message,
    create_update_message,
    handle_sync_message,
)
from websockets.sync.client import connect

SERVER_ENV = "SERVER"
REPO = Path(__file__).resolve().parents[2]
WORK = REPO / "bench" / "work" / "correctness"
HEADER = b"0.0.1\0"
COOKIE = "fastapiusersauth"
EMAIL = "gate@test.org"
PASSWORD = "pwd"
SYNC_TIMEOUT = float(os.environ.get("SYNC_TIMEOUT", "120"))

CLIS = {
    "rust": Path(
        os.environ.get("COCAT_RUST_CLI", REPO / "target" / "release" / "cocat-server")
    ),
    "python": Path(
        os.environ.get(
            "COCAT_PYTHON_CLI",
            "/home/jeandet/Documents/prog/cocat-python-tests/.venv/bin/cocat",
        )
    ),
}


def server_name() -> str:
    """The server this gate runs against, `rust` unless `SERVER` says otherwise."""
    name = os.environ.get(SERVER_ENV, "rust")
    if name not in CLIS:
        raise SystemExit(f"SERVER must be one of {sorted(CLIS)}, not {name!r}")
    return name


def cli(name: str) -> Path:
    path = CLIS[name]
    if not path.exists():
        raise SystemExit(f"{path} does not exist; build it first")
    return path


def workdir(gate: str, name: str) -> Path:
    """A fresh directory for one gate run; the old one is dropped, not merged."""
    directory = WORK / f"{gate}-{name}"
    if directory.exists():
        subprocess.run(["rm", "-rf", str(directory)], check=True)
    (directory / "updates").mkdir(parents=True)
    return directory


def seed(db_path: Path, name: str, rooms: list[str]) -> None:
    """A database with one user who owns `rooms`, created by this server's CLI."""
    db_path.unlink(missing_ok=True)
    Path(f"{db_path}.secret").unlink(missing_ok=True)
    run(
        cli(name),
        "create-user",
        "--email",
        EMAIL,
        "--password",
        PASSWORD,
        "--db_path",
        str(db_path),
    )
    for room in rooms:
        run(
            cli(name),
            "create-room",
            "--owner",
            EMAIL,
            "--room-id",
            room,
            "--db_path",
            str(db_path),
        )


def run(*command: object) -> None:
    subprocess.run([str(part) for part in command], check=True, capture_output=True)


def free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


class Server:
    """One `serve` process, started and stopped by the gate that needed it."""

    def __init__(self, name: str, update_dir: Path, db_path: Path) -> None:
        self.name = name
        self.host = "127.0.0.1"
        self.port = free_port()
        self.update_dir = update_dir
        self.db_path = db_path
        self.process: subprocess.Popen | None = None

    @property
    def base_url(self) -> str:
        return f"http://{self.host}:{self.port}"

    def start(self) -> "Server":
        self.process = subprocess.Popen(
            [
                str(cli(self.name)),
                "serve",
                "--host",
                self.host,
                "--port",
                str(self.port),
                "--update_dir",
                str(self.update_dir),
                "--db_path",
                str(self.db_path),
            ]
        )
        self._wait_ready()
        return self

    def _wait_ready(self) -> None:
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if self.process is not None and self.process.poll() is not None:
                raise SystemExit(f"the server exited with {self.process.returncode}")
            try:
                with urlopen(self.base_url, timeout=1):
                    return
            except HTTPError:
                return
            except URLError:
                time.sleep(0.05)
        raise SystemExit("the server did not answer within 30 s")

    def stop(self) -> None:
        if self.process is not None and self.process.poll() is None:
            self.process.send_signal(signal.SIGINT)
            self.process.wait(timeout=60)

    def __enter__(self) -> Self:
        return self.start()

    def __exit__(self, *_: object) -> None:
        self.stop()


def login(server: Server, email: str = EMAIL, password: str = PASSWORD) -> str:
    data = urlencode({"username": email, "password": password}).encode()
    request = Request(server.base_url + "/auth/jwt/login", data=data, method="POST")
    with urlopen(request, timeout=10) as response:
        for header in response.headers.get_all("set-cookie") or []:
            name, _, value = header.partition("=")
            if name == COOKIE:
                return value.split(";")[0]
    raise SystemExit("the login response carried no session cookie")


def open_socket(server: Server, room: str, cookie: str):
    return connect(
        f"ws://{server.host}:{server.port}/room/{room}",
        additional_headers={"cookie": f"{COOKIE}={cookie}"},
        open_timeout=10,
        max_size=None,
        legacy=True,
    )


def sync_into(document: Doc, socket, timeout: float = SYNC_TIMEOUT) -> None:
    socket.send(create_sync_message(document))
    while True:
        frame = socket.recv(timeout=timeout)
        if frame[0] == YMessageType.SYNC and frame[1] == YSyncMessageType.SYNC_STEP2:
            handle_sync_message(frame[1:], document)
            return


def fresh_database(socket, timeout: float = SYNC_TIMEOUT) -> DB:
    document = Doc()
    sync_into(document, socket, timeout)
    return DB(doc=document)


def apply(frame: bytes, document: Doc) -> None:
    """Apply one frame the server sent; anything but a sync frame is a protocol bug."""
    if frame[0] != YMessageType.SYNC:
        raise SystemExit(f"unexpected message type {frame[0]}")
    handle_sync_message(frame[1:], document)


def root_state(document: Doc) -> dict[str, object]:
    """The room's data as plain JSON: the two root maps, nothing else."""
    return {
        name: document.get(name, type=Map).to_py() for name in sorted(document.keys())
    }


def state_json(document: Doc) -> str:
    return json.dumps(root_state(document), sort_keys=True)


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


class RoomFile:
    """Builds a room file the way the server writes one: a record per update."""

    def __init__(self) -> None:
        self.database = DB()
        self.updates: list[bytes] = []
        self.database.doc.observe(lambda event: self.updates.append(event.update))

    def write(self, path: Path) -> Path:
        with path.open("wb") as file:
            file.write(HEADER)
            for update in self.updates:
                file.write(varuint(len(update)))
                file.write(update)
        return path


def read_room(path: Path) -> Doc:
    """The document a room file holds, applied to a fresh `Doc` by pycrdt alone."""
    data = path.read_bytes()
    if not data.startswith(HEADER):
        raise SystemExit(f"{path} is not a cocat room file")
    document = Doc()
    position = len(HEADER)
    while position < len(data):
        length, position = read_varuint(data, position)
        document.apply_update(data[position : position + length])
        position += length
    return document


def publish(socket, database: DB) -> None:
    """Send every local update to the server, without reading anything back."""
    database.doc.observe(lambda event: socket.send(create_update_message(event.update)))


class Live:
    """A socket whose document publishes every local update, and waits for its echo.

    The server broadcasts an applied update back to the client that sent it, so
    "how many frames did the server owe me" is exact: one per local update. Waiting
    for them is the sync point; no sleep guesses when the server is done.
    """

    def __init__(self, socket, database: DB) -> None:
        self.socket = socket
        self.database = database
        self.sent = 0
        self.received = 0
        database.doc.observe(lambda event: self._publish(event.update))

    def _publish(self, update: bytes) -> None:
        self.socket.send(create_update_message(update))
        self.sent += 1

    def sync_to(self, target: Doc, timeout: float = SYNC_TIMEOUT) -> int:
        """Read frames until this client holds `target`'s state; return how many.

        The server's broadcasts are the only source of truth here: counting the
        frames a write is *expected* to produce assumes every local transaction
        leaves the room's document changed, which an empty update does not.
        """
        frames = 0
        wanted = state_json(target)
        while state_json(self.database.doc) != wanted:
            apply(self.socket.recv(timeout=timeout), self.database.doc)
            self.received += 1
            frames += 1
        return frames

    def pull(self, count: int, timeout: float = SYNC_TIMEOUT) -> None:
        """Read and apply `count` frames the server broadcast to this socket."""
        for _ in range(count):
            apply(self.socket.recv(timeout=timeout), self.database.doc)
            self.received += 1

    def close(self) -> None:
        self.socket.close()


def read_varuint(data: bytes, position: int) -> tuple[int, int]:
    value = 0
    shift = 0
    while True:
        byte = data[position]
        position += 1
        value |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return value, position
        shift += 7
