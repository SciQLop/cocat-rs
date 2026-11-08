"""A refused HTTP call raises a class naming why, with the server's own text."""

import pytest
from conftest import PASSWORD, login, run

import cocat_rs as cc


def viewer_cookie(server):
    email = "viewer@example.org"
    database = str(server.directory / "cocat.db")
    run("create-user", "--email", email, "--password", PASSWORD, "--db_path", database)
    run(
        "add-user-to-room",
        "--email",
        email,
        "--room_id",
        server.room_id,
        "--role",
        "viewer",
        "--db_path",
        database,
    )
    return login(server.url, email)


def test_server_errors_stay_value_errors():
    for kind in (cc.Unauthorized, cc.Forbidden, cc.ServerNotFound, cc.Conflict):
        assert issubclass(kind, cc.ServerError)
    assert issubclass(cc.ServerError, ValueError)
    assert issubclass(cc.ServerNotFound, cc.NotFound)


def test_an_absent_session_is_unauthorized(server):
    with pytest.raises(cc.Unauthorized) as refused:
        cc.rooms(server.url, "not-a-session")

    assert refused.value.status == 401
    assert refused.value.detail
    assert str(refused.value) == refused.value.detail
    assert isinstance(refused.value, ValueError)


def test_a_viewer_saving_a_version_is_forbidden(server):
    with pytest.raises(cc.Forbidden) as refused:
        cc.commit(server.url, viewer_cookie(server), server.room_id)

    assert refused.value.status == 403
    assert refused.value.detail


def test_an_unknown_room_is_not_found(server):
    with pytest.raises(cc.ServerNotFound) as refused:
        cc.history(server.url, server.cookie, "no-such-room")

    assert refused.value.status == 404
    assert refused.value.detail
    assert isinstance(refused.value, cc.NotFound)


def test_saving_without_changes_is_a_conflict(server):
    with pytest.raises(cc.Conflict) as refused:
        cc.commit(server.url, server.cookie, server.room_id)

    assert refused.value.status == 409
    assert refused.value.detail == "Nothing changed since the last saved version."
    assert str(refused.value) == "Nothing changed since the last saved version."


def test_a_bad_password_is_a_plain_server_error(server):
    with pytest.raises(cc.ServerError) as refused:
        cc.login(server.url, server.email, "wrong")

    assert type(refused.value) is cc.ServerError
    assert refused.value.status == 400
    assert refused.value.detail == "LOGIN_BAD_CREDENTIALS"
