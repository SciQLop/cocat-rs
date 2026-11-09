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
    cookie = cc.login(server.url, server.email, "plum orbit lantern cello")
    assert cookie
    assert ";" not in cookie


def test_a_bad_password_is_refused(server):
    with pytest.raises(cc.Unauthorized, match="wrong email or password"):
        cc.login(server.url, server.email, "wrong")


def test_projects_lists_the_callers_projects_with_their_roles(server):
    projects = cc.projects(server.url, server.cookie)
    assert (server.project_id, "bench", "owner") in [
        (project.id, project.name, project.role) for project in projects
    ]


def test_projects_flag_the_own_project(server):
    projects = cc.projects(server.url, server.cookie)
    own = [project for project in projects if project.personal]
    assert [project.role for project in own] == ["owner"]
    assert own[0].id != server.project_id
    assert not next(project for project in projects if project.id == server.project_id).personal


def test_a_refused_handshake_is_unauthorized_and_set_cookie_recovers(server, cache):
    project = cc.Project.open(str(cache), server.project_id)
    try:
        project.connect(server.url, "not-a-session")
        wait_until(lambda: project.status == "unauthorized")
        assert project.status == "unauthorized"

        project.set_cookie(server.cookie)
        project.wait_synced(15)
        assert project.status == "synced"
    finally:
        project.close()


def test_credentials_log_in_again_when_the_cookie_is_stale(server, cache):
    project = cc.Project.open(str(cache), server.project_id)
    try:
        project.connect(server.url, "not-a-session", (server.email, "plum orbit lantern cello"))
        project.wait_synced(15)
        assert project.status == "synced"
    finally:
        project.close()


def test_credentials_alone_connect(server, cache):
    project = cc.Project.open(str(cache), server.project_id)
    try:
        project.connect(server.url, credentials=(server.email, "plum orbit lantern cello"))
        project.wait_synced(15)
        assert project.status == "synced"
    finally:
        project.close()


def test_a_wrong_password_is_refused_at_connect(server, cache):
    project = cc.Project.open(str(cache), server.project_id)
    try:
        with pytest.raises(cc.Unauthorized):
            project.connect(server.url, credentials=(server.email, "wrong"))
        assert project.status == "offline"
    finally:
        project.close()


def test_connect_needs_a_cookie_or_credentials(server, cache):
    project = cc.Project.open(str(cache), server.project_id)
    try:
        with pytest.raises(ValueError, match="cookie or credentials"):
            project.connect(server.url)
    finally:
        project.close()


@pytest.mark.asyncio
async def test_synced_awaits_the_first_sync(server, cache):
    project = cc.Project.open(str(cache), server.project_id)
    try:
        project.connect(server.url, server.cookie)
        await project.synced()
        assert project.status == "synced"
    finally:
        project.close()
