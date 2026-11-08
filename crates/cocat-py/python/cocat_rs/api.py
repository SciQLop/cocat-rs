"""The script mode: the synchronous API Python cocat's scripts already use.

`log_in`, `get_catalogue`, `save` and the rest keep Python cocat's names,
arguments and errors, so a scientist's script runs against a Rust server by
changing the import. The session holds one `cocat_rs.Room`; every call is a
local write the sync task sends, and `save()` is the only one that waits for the
server to acknowledge it.
"""

from __future__ import annotations

import atexit
import contextlib
import urllib.request
from pathlib import Path
from urllib.parse import urlparse

import keyring

from . import Room
from ._native import RoomInfo, export_votable as _export_votable
from ._native import login as _login
from ._native import rooms as _rooms


CONNECT_TIMEOUT = 30.0
REFRESH_TIMEOUT = 30.0


class Session:
    """The room and the connection a script's module-level calls act on."""

    def __init__(self) -> None:
        self.host = "http://localhost"
        self.port = 8000
        self.prefix = ""
        self.room_id: str | None = None
        self.file_path = "updates"
        self.cookie: str | None = None
        self.room: Room | None = None

    def base_url(self) -> str:
        return f"{self.host}:{self.port}/{self.prefix}"

    def set_config(
        self,
        host: str | None = None,
        port: int | None = None,
        file_path: str | None = None,
        room_id: str | None = None,
    ) -> None:
        if host is not None:
            parsed = urlparse(host)
            self.host = f"{parsed.scheme}://{parsed.hostname}"
            if parsed.port is not None:
                self.port = parsed.port
            prefix = parsed.path.strip("/")
            self.prefix = f"{prefix}/" if prefix else ""
        if port is not None:
            self.port = port
        if file_path is not None:
            self.file_path = file_path
        if room_id is not None:
            self.room_id = room_id


SESSION = Session()


def _reset() -> None:
    """Forgets the session, room and credentials of the previous script."""
    global SESSION
    SESSION = Session()


def set_config(
    *,
    host: str | None = None,
    port: int | None = None,
    file_path: str | None = None,
    room_id: str | None = None,
) -> None:
    """Points the session at a server, a cache directory and maybe a room."""
    SESSION.set_config(host=host, port=port, file_path=file_path, room_id=room_id)


def check_connected() -> Room:
    """The room every script call needs, or the error Python cocat raises."""
    if SESSION.room is None:
        raise RuntimeError("Not logged in")
    return SESSION.room


def _connect() -> None:
    if SESSION.cookie is None:
        raise RuntimeError("Username or password not provided")
    if SESSION.room_id is None:
        SESSION.room_id = _default_room(SESSION.base_url(), SESSION.cookie)
    room = Room.open(str(Path(SESSION.file_path)), SESSION.room_id)
    room.connect(SESSION.base_url(), SESSION.cookie)
    room.wait_synced(CONNECT_TIMEOUT)
    SESSION.room = room
    atexit.register(save_on_exit)


def log_in(
    username: str | None = None,
    password: str | None = None,
    save_credentials: bool = True,
    connect: bool = True,
) -> None:
    """Logs in, keeping the credentials in the keyring when one is available."""
    base_url = SESSION.base_url()
    credential = None
    if save_credentials:
        with contextlib.suppress(Exception):
            credential = keyring.get_credential(base_url, username)
    if credential is not None:
        username = username or credential.username
        password = password or credential.password
    if not username or not password:
        raise RuntimeError("Username or password not provided")
    if save_credentials:
        with contextlib.suppress(Exception):
            keyring.set_password(base_url, username, password)
    try:
        SESSION.cookie = _login(base_url, username, password)
    except ValueError as error:
        raise RuntimeError("Wrong username or password") from error
    if connect:
        _connect()


def log_out() -> None:
    """Forgets the cookie and closes the room; the cache stays for the next run."""
    cookie = SESSION.cookie
    if cookie is not None:
        with contextlib.suppress(Exception):
            request = urllib.request.Request(
                f"{SESSION.base_url()}auth/jwt/logout", method="POST"
            )
            request.add_header("cookie", f"fastapiusersauth={cookie}")
            urllib.request.urlopen(request, timeout=5.0).close()
    if SESSION.room is not None:
        SESSION.room.close()
    SESSION.cookie = None
    SESSION.room = None


def create_catalogue(
    *,
    name: str,
    author: str | None = None,
    uuid: str | None = None,
    tags: object = None,
    attributes: dict | None = None,
    events: object = None,
):
    return check_connected().create_catalogue(
        name=name,
        author=author,
        uuid=uuid,
        tags=tags,
        attributes=attributes,
        events=events,
    )


def create_event(
    *,
    start,
    stop,
    author: str | None = None,
    uuid: str | None = None,
    tags: object = None,
    products: object = None,
    attributes: dict | None = None,
    **fields,
):
    """Creates an event; `fields` are the other keyword arguments of `Room.create_event`."""
    return check_connected().create_event(
        start=start,
        stop=stop,
        author=author,
        uuid=uuid,
        tags=tags,
        products=products,
        attributes=attributes,
        **fields,
    )


def get_catalogue(uuid_or_name: str):
    return check_connected().catalogue(uuid_or_name)


def get_event(uuid: str):
    return check_connected().event(uuid)


def refresh() -> None:
    """Waits for the room to catch up with the server."""
    # Already synced, so waiting for sync returns at once: a round trip with the server
    # is what brings back what changed since.
    check_connected().save(REFRESH_TIMEOUT)


def save(timeout: float | None = None) -> None:
    """Waits for the server to acknowledge every local change made so far."""
    check_connected().save(timeout)


def import_votable(file_path: str | Path, table_name: str | None = None) -> set:
    """Adds the catalogues of a VOTable file to the room, as Python cocat does."""
    room = check_connected()
    room.import_votable(str(file_path), table_name)
    return set(room.catalogues())


def export_votable(catalogues, file_path: str | Path) -> None:
    """Writes the catalogues to a VOTable file."""
    Path(file_path).write_text(_export_votable(catalogues), encoding="utf-8")


def save_on_exit() -> None:
    """Asks before saving, as Python cocat does when the interpreter exits."""
    if SESSION.room is None:
        return
    answer = input("Save changes (Y/n)? ")
    reply = answer.strip().lower()
    if reply == "" or reply.startswith("y"):
        save()
        print("Changes have been saved.")


def _list_rooms(base_url: str, cookie: str) -> list[RoomInfo]:
    try:
        return _rooms(base_url, cookie)
    except ValueError as error:
        raise RuntimeError(f"Cannot list rooms: {error}") from error


def _default_room(base_url: str, cookie: str) -> str:
    """The user's own room, else the first room the user belongs to."""
    rooms = _list_rooms(base_url, cookie)
    if not rooms:
        raise RuntimeError("No room available; create one or accept an invitation")
    return next((room.id for room in rooms if room.personal), rooms[0].id)
