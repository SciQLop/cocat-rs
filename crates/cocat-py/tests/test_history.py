import datetime
import time

import pytest

import cocat_rs as cc

UTC = datetime.timezone.utc
START = datetime.datetime(2025, 1, 31, 10, 0, tzinfo=UTC)
STOP = datetime.datetime(2025, 1, 31, 10, 1, tzinfo=UTC)


def wait_until(predicate, timeout=15.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.02)
    raise AssertionError("the condition did not hold in time")


def holds(project, uuid):
    try:
        project.event(uuid)
    except cc.NotFound:
        return False
    return True


def history(server):
    return cc.history(server.url, server.cookie, server.project_id)


def two_versions(server, project, colleague):
    """The owner adds `a`, then the colleague changes it and adds `b`."""
    a = project.create_event(start=START, stop=STOP)
    project.save(15)
    wait_until(lambda: holds(colleague, a.uuid))
    colleague.event(a.uuid).confidence = 0.9
    b = colleague.create_event(start=START, stop=STOP)
    colleague.save(15)
    first, second = sorted(version["id"] for version in history(server))
    return a, b, first, second


def test_history_lists_a_version_by_the_writer(server, project):
    project.create_event(start=START, stop=STOP)
    project.save(15)

    versions = history(server)

    assert len(versions) == 1
    assert versions[0]["author_email"] == server.email
    assert versions[0]["events_added"] == 1
    assert versions[0]["name"] is None


def test_project_at_a_version_shows_the_past(server, project, colleague):
    a, b, first, _ = two_versions(server, project, colleague)

    past = cc.Project.at(server.url, server.cookie, server.project_id, first)

    assert past.status == "offline"
    assert past.event(a.uuid).confidence is None
    with pytest.raises(cc.NotFound):
        past.event(b.uuid)


def test_a_past_project_is_read_only(server, project):
    a = project.create_event(start=START, stop=STOP)
    catalogue = project.create_catalogue(name="alpha")
    project.save(15)
    (version,) = [version["id"] for version in history(server)]

    past = cc.Project.at(server.url, server.cookie, server.project_id, version)

    assert past.read_only
    assert past.role is None
    read_only = "this project is read-only: it is a past version"
    with pytest.raises(RuntimeError, match=read_only):
        past.create_event(start=START, stop=STOP)
    with pytest.raises(RuntimeError, match=read_only):
        past.event(a.uuid).confidence = 0.5
    with pytest.raises(RuntimeError, match=read_only):
        past.catalogue(catalogue.uuid).name = "beta"
    with pytest.raises(RuntimeError, match=read_only):
        past.connect(server.url, server.cookie)
    assert past.event(a.uuid).confidence is None


def test_restore_from_python_undoes_an_edit(server, project, colleague):
    a, b, first, _ = two_versions(server, project, colleague)

    assert cc.restore(server.url, server.cookie, server.project_id, first)

    wait_until(lambda: project.event(a.uuid).confidence is None)
    wait_until(lambda: project.event(b.uuid).status == "deleted")
    assert history(server)[0]["author_email"] == server.email
    assert history(server)[0]["restored_from"] == first
    assert not cc.restore(server.url, server.cookie, server.project_id, first)


def test_a_named_version_comes_back_named(server, project):
    project.create_event(start=START, stop=STOP)
    project.save(15)
    (version,) = [version["id"] for version in history(server)]

    cc.name_version(server.url, server.cookie, server.project_id, version, "paper v1")
    assert history(server)[0]["name"] == "paper v1"

    cc.name_version(server.url, server.cookie, server.project_id, version, None)
    assert history(server)[0]["name"] is None


def test_a_version_keeps_its_id_and_name_while_its_author_keeps_editing(server, project):
    project.create_event(start=START, stop=STOP)
    project.save(15)
    (version,) = [version["id"] for version in history(server)]
    cc.name_version(server.url, server.cookie, server.project_id, version, "paper v1")

    project.create_event(start=START, stop=STOP)
    project.save(15)

    versions = history(server)
    assert [version["id"] for version in versions] == [version]
    assert versions[0]["name"] == "paper v1"
    assert versions[0]["events_added"] == 2


def test_a_catalogue_filter_narrows_the_history(server, project, colleague):
    _, b, first, second = two_versions(server, project, colleague)
    catalogue = project.create_catalogue(name="alpha", events=[b])
    project.save(15)

    versions = cc.history(
        server.url, server.cookie, server.project_id, catalogue=catalogue.uuid
    )

    assert first not in [version["id"] for version in versions]
    assert second in [version["id"] for version in versions]


def test_saving_a_version_names_it_and_ends_it(server, project):
    project.create_event(start=START, stop=STOP)
    project.save(15)

    version = cc.commit(
        server.url, server.cookie, server.project_id, name="checked", message="all good"
    )
    project.create_event(start=START, stop=STOP)
    project.save(15)

    versions = history(server)
    assert [v["id"] for v in versions][1] == version
    assert versions[1]["committed"] is True
    assert versions[1]["name"] == "checked"
    assert versions[1]["message"] == "all good"
    assert versions[0]["committed"] is False


def test_saving_twice_without_changes_is_refused(server, project):
    project.create_event(start=START, stop=STOP)
    project.save(15)
    cc.commit(server.url, server.cookie, server.project_id)

    with pytest.raises(cc.Conflict, match="Nothing changed"):
        cc.commit(server.url, server.cookie, server.project_id)


def test_copying_a_version_makes_a_new_project_holding_it(server, project, colleague):
    a, b, first, _ = two_versions(server, project, colleague)
    before = history(server)

    copy = cc.copy_version(server.url, server.cookie, server.project_id, first, "try")

    assert copy != server.project_id
    assert "try" in [info.name for info in cc.projects(server.url, server.cookie)]
    (version,) = cc.history(server.url, server.cookie, copy)
    copied = cc.Project.at(server.url, server.cookie, copy, version["id"])
    assert copied.event(a.uuid).confidence is None
    with pytest.raises(cc.NotFound):
        copied.event(b.uuid)
    assert version["name"].startswith("Copied from ")
    assert history(server) == before
    assert project.event(a.uuid).confidence == 0.9
