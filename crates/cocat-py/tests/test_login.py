import time

import pytest

import cocat_rs as cc


def wait_until(predicate, timeout=15.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.02)
    raise AssertionError("the condition did not hold in time")


def test_login_returns_the_session_cookie(server):
    cookie = cc.login(server.url, server.email, "hunter2")
    assert cookie
    assert ";" not in cookie


def test_a_bad_password_is_refused(server):
    with pytest.raises(ValueError):
        cc.login(server.url, server.email, "wrong")


def test_rooms_lists_the_callers_rooms_with_their_roles(server):
    rooms = cc.rooms(server.url, server.cookie)
    assert (server.room_id, "bench", "owner") in [
        (room.id, room.name, room.role) for room in rooms
    ]


def test_rooms_flag_the_own_project(server):
    rooms = cc.rooms(server.url, server.cookie)
    own = [room for room in rooms if room.personal]
    assert [room.role for room in own] == ["owner"]
    assert own[0].id != server.room_id
    assert not next(room for room in rooms if room.id == server.room_id).personal


def test_a_refused_handshake_is_unauthorized_and_set_cookie_recovers(server, cache):
    room = cc.Room.open(str(cache), server.room_id)
    try:
        room.connect(server.url, "not-a-session")
        wait_until(lambda: room.status == "unauthorized")
        assert room.status == "unauthorized"

        room.set_cookie(server.cookie)
        room.wait_synced(15)
        assert room.status == "synced"
    finally:
        room.close()


def test_credentials_log_in_again_when_the_cookie_is_stale(server, cache):
    room = cc.Room.open(str(cache), server.room_id)
    try:
        room.connect(server.url, "not-a-session", (server.email, "hunter2"))
        room.wait_synced(15)
        assert room.status == "synced"
    finally:
        room.close()


@pytest.mark.asyncio
async def test_synced_awaits_the_first_sync(server, cache):
    room = cc.Room.open(str(cache), server.room_id)
    try:
        room.connect(server.url, server.cookie)
        await room.synced()
        assert room.status == "synced"
    finally:
        room.close()
