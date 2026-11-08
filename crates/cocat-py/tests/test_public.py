"""Public catalogues: every account reads them, curators write them."""

import time

import pytest
from conftest import PASSWORD, login, run

import cocat_rs as cc

READ_ONLY = r"this project is read-only for you \(viewer\)"


def account(server, email, *extra):
    database = str(server.directory / "cocat.db")
    run("create-user", "--email", email, "--password", PASSWORD, "--db_path", database)
    for command in extra:
        run(command, "--email", email, "--db_path", database)
    return login(server.url, email)


def connected(directory, project_id, url, cookie):
    directory.mkdir()
    project = cc.Project.open(str(directory), project_id)
    project.connect(url, cookie)
    project.wait_synced(15)
    return project


def wait_until(predicate, timeout=15.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.02)
    raise AssertionError("the condition did not hold in time")


def test_only_a_curator_creates_one_and_everyone_lists_it(server):
    with pytest.raises(cc.Forbidden):
        cc.create_public_catalogue(server.url, server.cookie, "Mine")
    curator = account(server, "curator@example.org", "set-curator")

    made = cc.create_public_catalogue(
        server.url,
        curator,
        "Shocks",
        folder="AMDA/Solar wind",
        source="AMDA",
        description="Shocks seen by ACE",
        key="amda.sharedcatalog_22",
    )

    assert made == "public.amda.sharedcatalog_22"
    with pytest.raises(cc.Conflict):
        cc.create_public_catalogue(server.url, curator, "Again", key="amda.sharedcatalog_22")
    [entry] = cc.public_catalogues(server.url, server.cookie)
    assert entry["id"] == made
    assert entry["name"] == "Shocks"
    assert entry["folder"] == "AMDA/Solar wind"
    assert entry["source"] == "AMDA"
    assert entry["description"] == "Shocks seen by ACE"
    assert entry["role"] == "viewer"
    assert cc.public_catalogues(server.url, curator)[0]["role"] == "editor"
    assert made not in [info.id for info in cc.projects(server.url, server.cookie)]


def test_a_curator_writes_and_a_reader_gets_a_read_only_copy(server, tmp_path):
    curator = account(server, "curator@example.org", "set-curator")
    public_id = cc.create_public_catalogue(server.url, curator, "Shocks")

    writer = connected(tmp_path / "curator", public_id, server.url, curator)
    assert writer.role == "editor"
    assert not writer.read_only
    writer.create_catalogue(name="ACE shocks")
    writer.save(15)
    writer.close()

    reader = connected(tmp_path / "reader", public_id, server.url, server.cookie)
    wait_until(lambda: [c.name for c in reader.catalogues()] == ["ACE shocks"])
    assert reader.role == "viewer"
    assert reader.read_only
    with pytest.raises(RuntimeError, match=READ_ONLY):
        reader.create_catalogue(name="mine")
    reader.close()


def test_nobody_shares_one(server):
    curator = account(server, "curator@example.org", "set-curator")
    public_id = cc.create_public_catalogue(server.url, curator, "Shocks")

    for cookie in (server.cookie, curator):
        with pytest.raises(cc.Conflict):
            cc.members(server.url, cookie, public_id)
        with pytest.raises(cc.Conflict):
            cc.invite(server.url, cookie, public_id, role="viewer")
