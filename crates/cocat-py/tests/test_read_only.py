"""A viewer's project is read-only in the client, online and offline."""

import logging
import time

import pytest
from conftest import PASSWORD, login, run

import cocat_rs as cc

READ_ONLY = r"this project is read-only for you \(viewer\)"


def viewer_cookie(server):
    email = "viewer@example.org"
    database = str(server.directory / "cocat.db")
    run("create-user", "--email", email, "--password", PASSWORD, "--db_path", database)
    run(
        "add-user-to-room",
        "--email",
        email,
        "--room_id",
        server.project_id,
        "--role",
        "viewer",
        "--db_path",
        database,
    )
    return login(server.url, email)


def names(project):
    return sorted(catalogue.name for catalogue in project.catalogues())


def wait_until(predicate, timeout=15.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.02)
    raise AssertionError("the condition did not hold in time")


@pytest.fixture
def shared(server, project):
    """The owner's project, holding one catalogue the server has."""
    project.create_catalogue(name="shared")
    project.save(15)
    return project


@pytest.fixture
def viewer_cache(tmp_path):
    directory = tmp_path / "viewer"
    directory.mkdir()
    return str(directory)


def connected(server, directory, cookie):
    project = cc.Project.open(directory, server.project_id)
    project.connect(server.url, cookie)
    project.wait_synced(15)
    return project


def test_a_viewers_write_is_refused_and_never_cached(server, shared, viewer_cache):
    cookie = viewer_cookie(server)
    viewer = connected(server, viewer_cache, cookie)
    wait_until(lambda: names(viewer) == ["shared"])

    assert viewer.role == "viewer"
    assert viewer.read_only
    with pytest.raises(RuntimeError, match=READ_ONLY):
        viewer.create_catalogue(name="phantom")
    with pytest.raises(RuntimeError, match=READ_ONLY):
        viewer.catalogue("shared").name = "renamed"
    assert names(viewer) == ["shared"]
    viewer.close()

    reopened = connected(server, viewer_cache, cookie)
    assert names(reopened) == ["shared"]
    reopened.close()


def test_a_cached_viewer_project_opens_read_only_offline(server, shared, viewer_cache):
    connected(server, viewer_cache, viewer_cookie(server)).close()

    offline = cc.Project.open(viewer_cache, server.project_id)

    assert offline.status == "offline"
    assert offline.role == "viewer"
    assert offline.read_only
    with pytest.raises(RuntimeError, match=READ_ONLY):
        offline.create_catalogue(name="phantom")
    offline.close()


def test_a_project_that_never_connected_has_no_role(cache):
    project = cc.Project.open(str(cache), "never-connected")

    assert project.role is None
    assert not project.read_only
    project.create_catalogue(name="mine")
    project.close()


def test_local_changes_of_a_viewer_are_dropped_on_connect(
    server, shared, viewer_cache, caplog
):
    caplog.set_level(logging.WARNING)
    drifted = cc.Project.open(viewer_cache, server.project_id)
    drifted.create_catalogue(name="phantom")
    drifted.close()

    viewer = connected(server, viewer_cache, viewer_cookie(server))
    wait_until(lambda: names(viewer) == ["shared"])
    viewer.close()

    assert names(cc.Project.open(viewer_cache, server.project_id)) == ["shared"]
    warnings = [
        record.getMessage()
        for record in caplog.records
        if record.name.startswith("cocat_client") and "read" in record.getMessage()
    ]
    assert len(warnings) == 1, warnings


def test_an_owner_writes_as_before(server, shared, colleague):
    assert shared.role == "owner"
    assert not shared.read_only
    assert colleague.role == "editor"
    assert not colleague.read_only

    colleague.create_catalogue(name="by the editor")
    colleague.save(15)

    wait_until(lambda: names(shared) == ["by the editor", "shared"])


def test_a_promotion_while_connected_is_picked_up_with_credentials(
    server, shared, viewer_cache
):
    cookie = viewer_cookie(server)
    viewer = cc.Project.open(viewer_cache, server.project_id)
    viewer.connect(server.url, cookie, credentials=("viewer@example.org", PASSWORD))
    viewer.wait_synced(15)
    assert viewer.read_only
    members = cc.members(server.url, server.cookie, server.project_id)
    user_id = next(m["user_id"] for m in members if m["email"] == "viewer@example.org")

    cc.set_role(server.url, server.cookie, server.project_id, user_id, "editor")

    wait_until(lambda: viewer.role == "editor")
    assert not viewer.read_only
    viewer.create_catalogue(name="now an editor")
    viewer.close()
