"""The script mode: the synchronous API Python cocat's scripts already use.

`log_in`, `get_catalogue`, `save` and the rest keep Python cocat's names,
arguments and errors, so a scientist's script runs against a Rust server by
changing the import. The session holds one `cocat_rs.Project`; every call is a
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

from . import Project
from ._old_names import renamed_keywords
from ._native import ProjectInfo, export_votable as _export_votable
from ._native import login as _login
from ._native import projects as _projects


CONNECT_TIMEOUT = 30.0
REFRESH_TIMEOUT = 30.0


class Session:
    """The project and the connection a script's module-level calls act on."""

    def __init__(self) -> None:
        self.host = "http://localhost"
        self.port = 8000
        self.prefix = ""
        self.project_id: str | None = None
        self.file_path = "updates"
        self.cookie: str | None = None
        self.project: Project | None = None

    def base_url(self) -> str:
        return f"{self.host}:{self.port}/{self.prefix}"

    def set_config(
        self,
        host: str | None = None,
        port: int | None = None,
        file_path: str | None = None,
        project_id: str | None = None,
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
        if project_id is not None:
            self.project_id = project_id


SESSION = Session()


def _reset() -> None:
    """Forgets the session, project and credentials of the previous script."""
    global SESSION
    SESSION = Session()


@renamed_keywords(room_id="project_id")
def set_config(
    *,
    host: str | None = None,
    port: int | None = None,
    file_path: str | None = None,
    project_id: str | None = None,
) -> None:
    """Points the session at a server, a cache directory and maybe a project."""
    SESSION.set_config(host=host, port=port, file_path=file_path, project_id=project_id)


def check_connected() -> Project:
    """The project every script call needs, or the error Python cocat raises."""
    if SESSION.project is None:
        raise RuntimeError("Not logged in")
    return SESSION.project


def _connect() -> None:
    if SESSION.cookie is None:
        raise RuntimeError("Username or password not provided")
    if SESSION.project_id is None:
        SESSION.project_id = _default_project(SESSION.base_url(), SESSION.cookie)
    project = Project.open(str(Path(SESSION.file_path)), SESSION.project_id)
    project.connect(SESSION.base_url(), SESSION.cookie)
    project.wait_synced(CONNECT_TIMEOUT)
    SESSION.project = project
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
    """Forgets the cookie and closes the project; the cache stays for the next run."""
    cookie = SESSION.cookie
    if cookie is not None:
        with contextlib.suppress(Exception):
            request = urllib.request.Request(
                f"{SESSION.base_url()}auth/jwt/logout", method="POST"
            )
            request.add_header("cookie", f"fastapiusersauth={cookie}")
            urllib.request.urlopen(request, timeout=5.0).close()
    if SESSION.project is not None:
        SESSION.project.close()
    SESSION.cookie = None
    SESSION.project = None


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
    """Creates an event; `fields` are the other keyword arguments of `Project.create_event`."""
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
    """Waits for the project to catch up with the server."""
    # Already synced, so waiting for sync returns at once: a round trip with the server
    # is what brings back what changed since.
    check_connected().save(REFRESH_TIMEOUT)


def save(timeout: float | None = None) -> None:
    """Waits for the server to acknowledge every local change made so far."""
    check_connected().save(timeout)


def import_votable(file_path: str | Path, table_name: str | None = None) -> set:
    """Adds the catalogues of a VOTable file to the project, as Python cocat does."""
    project = check_connected()
    project.import_votable(str(file_path), table_name)
    return set(project.catalogues())


def export_votable(catalogues, file_path: str | Path) -> None:
    """Writes the catalogues to a VOTable file."""
    Path(file_path).write_text(_export_votable(catalogues), encoding="utf-8")


def save_on_exit() -> None:
    """Asks before saving, as Python cocat does when the interpreter exits."""
    if SESSION.project is None:
        return
    answer = input("Save changes (Y/n)? ")
    reply = answer.strip().lower()
    if reply == "" or reply.startswith("y"):
        save()
        print("Changes have been saved.")


def _list_projects(base_url: str, cookie: str) -> list[ProjectInfo]:
    try:
        return _projects(base_url, cookie)
    except ValueError as error:
        raise RuntimeError(f"Cannot list projects: {error}") from error


def _default_project(base_url: str, cookie: str) -> str:
    """The user's own project, else the first project the user belongs to."""
    projects = _list_projects(base_url, cookie)
    if not projects:
        raise RuntimeError("No project available; create one or accept an invitation")
    return next((project.id for project in projects if project.personal), projects[0].id)
