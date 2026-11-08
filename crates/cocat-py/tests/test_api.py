"""The script mode, ported from Python cocat's tests/test_api.py.

Python cocat's own tests monkeypatch its keyring wallet; here the same in-memory
wallet replaces the keyring backend, and the server is the `cocat-server` binary
from `conftest.py`.
"""

from datetime import datetime

import pytest

import cocat_rs
from cocat_rs import api


class Credentials:
    def __init__(self, username: str, password: str) -> None:
        self.username = username
        self.password = password


WALLET: dict[str, Credentials] = {}


def get_credential(service_name: str, username: str | None) -> Credentials | None:
    return WALLET.get(service_name)


def set_password(service_name: str, username: str, password: str) -> None:
    WALLET[service_name] = Credentials(username, password)


@pytest.fixture(autouse=True)
def wallet(monkeypatch):
    """The in-memory keyring wallet, and a fresh session per test."""
    WALLET.clear()
    monkeypatch.setattr(api.keyring, "get_credential", get_credential)
    monkeypatch.setattr(api.keyring, "set_password", set_password)
    monkeypatch.setattr(api.atexit, "register", lambda function: None)
    api._reset()
    yield WALLET
    api._reset()


def configure(server, tmp_path, project_id=None) -> None:
    api.set_config(
        host=server.url,
        file_path=str(tmp_path / "cache"),
        project_id=project_id,
    )


def until(predicate, timeout: float = 15.0) -> None:
    """Wait for a change to come back from the server; the client syncs in the background."""
    import time

    deadline = time.monotonic() + timeout
    reason = None
    while True:
        try:
            if predicate():
                return
        except RuntimeError as error:
            reason = error
        if time.monotonic() > deadline:
            raise AssertionError(f"the change never arrived after {timeout}s: {reason}")
        time.sleep(0.05)


def test_user_project(server, tmp_path) -> None:
    configure(server, tmp_path)
    api.log_in(server.email, "hunter2")

    listed = cocat_rs.projects(api.SESSION.base_url(), api.SESSION.cookie)
    assert api.SESSION.project_id in [project.id for project in listed]
    assert api.SESSION.project_id != server.project_id
    assert len(api.SESSION.project_id) == 32


def test_api(server, tmp_path) -> None:
    configure(server, tmp_path, project_id=server.project_id)
    api.log_in(server.email, "hunter2")

    catalogue = api.create_catalogue(name="cat0", author="Paul")
    event = api.create_event(
        start="2025-01-30",
        stop="2026-01-30",
        author="Mike",
    )
    assert api.get_event(event.uuid) == event
    assert api.get_catalogue(catalogue.uuid) == catalogue

    api.save()

    other = cocat_rs.Project.open(str(tmp_path / "other"), server.project_id)
    other.connect(server.url, server.cookie)
    other.wait_synced(15)
    try:
        until(lambda: other.catalogue("cat0").name == "cat0")

        remote = other.create_event(
            start="2025-01-31",
            stop="2026-01-31",
            author="John",
        )
        api.refresh()
        until(lambda: api.get_event(remote.uuid) == remote)
    finally:
        other.close()

    api.log_out()

    with pytest.raises(RuntimeError, match="Wrong username or password"):
        api.log_in("foo", "bar")


def test_login(server, tmp_path) -> None:
    configure(server, tmp_path, project_id=server.project_id)

    with pytest.raises(RuntimeError, match="Not logged in"):
        api.refresh()

    with pytest.raises(RuntimeError, match="Username or password not provided"):
        api.log_in(server.email, connect=False)

    api.log_in(server.email, "hunter2", connect=False)
    api.log_in(connect=False)


def test_login_with_port_in_host(server, tmp_path) -> None:
    api.set_config(host=server.url, file_path=str(tmp_path / "cache"))

    with pytest.raises(RuntimeError, match="Not logged in"):
        api.refresh()

    assert api.SESSION.host == "http://127.0.0.1"
    assert api.SESSION.port == int(server.url.rsplit(":", 1)[1])


def test_atexit(server, tmp_path, monkeypatch, capsys) -> None:
    monkeypatch.setattr("builtins.input", lambda _: "yes")
    configure(server, tmp_path, project_id=server.project_id)
    api.log_in(server.email, "hunter2")
    api.create_catalogue(name="cat0", author="Paul Newton")
    api.save_on_exit()
    assert capsys.readouterr().out == "Changes have been saved.\n"


def test_default_project(monkeypatch) -> None:
    def unavailable(base_url, cookie):
        raise RuntimeError("Cannot list projects: HTTP 500")

    monkeypatch.setattr(api, "_list_projects", unavailable)
    with pytest.raises(RuntimeError, match="HTTP 500"):
        api._default_project("http://localhost:8000/", "cookie")

    monkeypatch.setattr(api, "_list_projects", lambda base_url, cookie: [])
    with pytest.raises(RuntimeError, match="No project available"):
        api._default_project("http://localhost:8000/", "cookie")

    monkeypatch.setattr(
        api,
        "_list_projects",
        lambda base_url, cookie: [
            cocat_rs.ProjectInfo(id="room1", name="Lab", role="editor")
        ],
    )
    assert api._default_project("http://localhost:8000/", "cookie") == "room1"


def test_default_project_is_the_own_one_whatever_its_name(monkeypatch) -> None:
    monkeypatch.setattr(
        api,
        "_list_projects",
        lambda base_url, cookie: [
            cocat_rs.ProjectInfo(id="shared", name="Personal", role="owner"),
            cocat_rs.ProjectInfo(id="mine", name="Renamed", role="owner", personal=True),
        ],
    )
    assert api._default_project("http://localhost:8000/", "cookie") == "mine"


def test_save_waits_for_the_server(server, tmp_path) -> None:
    configure(server, tmp_path, project_id=server.project_id)
    api.log_in(server.email, "hunter2")
    api.create_catalogue(name="ack", author="Paul")

    api.save()

    other = cocat_rs.Project.open(str(tmp_path / "other"), server.project_id)
    other.connect(server.url, server.cookie)
    other.wait_synced(15)
    try:
        assert other.catalogue("ack").name == "ack"
    finally:
        other.close()


def test_a_save_after_a_relogin_keeps_the_cache(server, tmp_path) -> None:
    configure(server, tmp_path, project_id=server.project_id)
    api.log_in(server.email, "hunter2")
    api.create_catalogue(name="kept", author="Paul")
    api.save()

    api.log_out()
    api.log_in(server.email, "hunter2")

    assert api.get_catalogue("kept").name == "kept"


def test_create_event_takes_naive_datetimes_as_utc(server, tmp_path) -> None:
    configure(server, tmp_path, project_id=server.project_id)
    api.log_in(server.email, "hunter2")

    event = api.create_event(
        start=datetime(2025, 3, 1, 10, 0, 0, 500000),
        stop=datetime(2025, 3, 1, 10, 1, 0),
        author="Paul",
    )

    assert event.start.utcoffset().total_seconds() == 0
    assert event.start.microsecond == 500000
