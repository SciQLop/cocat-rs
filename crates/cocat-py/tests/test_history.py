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


def holds(room, uuid):
    try:
        room.event(uuid)
    except cc.NotFound:
        return False
    return True


def history(server):
    return cc.history(server.url, server.cookie, server.room_id)


def two_versions(server, room, colleague):
    """The owner adds `a`, then the colleague changes it and adds `b`."""
    a = room.create_event(start=START, stop=STOP)
    room.save(15)
    wait_until(lambda: holds(colleague, a.uuid))
    colleague.event(a.uuid).confidence = 0.9
    b = colleague.create_event(start=START, stop=STOP)
    colleague.save(15)
    first, second = sorted(version["id"] for version in history(server))
    return a, b, first, second


def test_history_lists_a_version_by_the_writer(server, room):
    room.create_event(start=START, stop=STOP)
    room.save(15)

    versions = history(server)

    assert len(versions) == 1
    assert versions[0]["author_email"] == server.email
    assert versions[0]["events_added"] == 1
    assert versions[0]["name"] is None


def test_room_at_a_version_shows_the_past(server, room, colleague):
    a, b, first, _ = two_versions(server, room, colleague)

    past = cc.Room.at(server.url, server.cookie, server.room_id, first)

    assert past.status == "offline"
    assert past.event(a.uuid).confidence is None
    with pytest.raises(cc.NotFound):
        past.event(b.uuid)


def test_a_past_room_is_read_only(server, room):
    a = room.create_event(start=START, stop=STOP)
    catalogue = room.create_catalogue(name="alpha")
    room.save(15)
    (version,) = [version["id"] for version in history(server)]

    past = cc.Room.at(server.url, server.cookie, server.room_id, version)

    read_only = "a past version is read-only"
    with pytest.raises(RuntimeError, match=read_only):
        past.create_event(start=START, stop=STOP)
    with pytest.raises(RuntimeError, match=read_only):
        past.event(a.uuid).confidence = 0.5
    with pytest.raises(RuntimeError, match=read_only):
        past.catalogue(catalogue.uuid).name = "beta"
    with pytest.raises(RuntimeError, match=read_only):
        past.connect(server.url, server.cookie)
    assert past.event(a.uuid).confidence is None


def test_restore_from_python_undoes_an_edit(server, room, colleague):
    a, b, first, _ = two_versions(server, room, colleague)

    assert cc.restore(server.url, server.cookie, server.room_id, first)

    wait_until(lambda: room.event(a.uuid).confidence is None)
    wait_until(lambda: room.event(b.uuid).status == "deleted")
    assert history(server)[0]["author_email"] == server.email
    assert history(server)[0]["restored_from"] == first
    assert not cc.restore(server.url, server.cookie, server.room_id, first)


def test_a_named_version_comes_back_named(server, room):
    room.create_event(start=START, stop=STOP)
    room.save(15)
    (version,) = [version["id"] for version in history(server)]

    cc.name_version(server.url, server.cookie, server.room_id, version, "paper v1")
    assert history(server)[0]["name"] == "paper v1"

    cc.name_version(server.url, server.cookie, server.room_id, version, None)
    assert history(server)[0]["name"] is None


def test_a_version_keeps_its_id_and_name_while_its_author_keeps_editing(server, room):
    room.create_event(start=START, stop=STOP)
    room.save(15)
    (version,) = [version["id"] for version in history(server)]
    cc.name_version(server.url, server.cookie, server.room_id, version, "paper v1")

    room.create_event(start=START, stop=STOP)
    room.save(15)

    versions = history(server)
    assert [version["id"] for version in versions] == [version]
    assert versions[0]["name"] == "paper v1"
    assert versions[0]["events_added"] == 2


def test_a_catalogue_filter_narrows_the_history(server, room, colleague):
    _, b, first, second = two_versions(server, room, colleague)
    catalogue = room.create_catalogue(name="alpha", events=[b])
    room.save(15)

    versions = cc.history(
        server.url, server.cookie, server.room_id, catalogue=catalogue.uuid
    )

    assert first not in [version["id"] for version in versions]
    assert second in [version["id"] for version in versions]
