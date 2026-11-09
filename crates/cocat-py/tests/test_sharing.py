"""Managing a project from Python: creating one, its members, invitations and groups."""

import json
import urllib.request

import pytest
from conftest import PASSWORD, login, run

import cocat_rs as cc


def account(server, email, role=None):
    """A new account; with `role`, a member of the server's project in that role."""
    database = str(server.directory / "cocat.db")
    run("create-user", "--email", email, "--password", PASSWORD, "--db_path", database)
    if role is not None:
        run(
            "add-user-to-room",
            "--email",
            email,
            "--room_id",
            server.project_id,
            "--role",
            role,
            "--db_path",
            database,
        )
    return login(server.url, email)


def member(server, email):
    members = cc.raw.members(server.url, server.cookie, server.project_id)
    return next(item for item in members if item["email"] == email)


def call(server, method, path, body):
    request = urllib.request.Request(
        f"{server.url}{path}",
        data=json.dumps(body).encode(),
        method=method,
        headers={
            "content-type": "application/json",
            "cookie": f"fastapiusersauth={server.cookie}",
        },
    )
    with urllib.request.urlopen(request) as response:
        return json.loads(response.read())


def test_a_created_project_is_owned_and_listed(server):
    created = cc.raw.create_project(server.url, server.cookie, "Bepi MSA")

    assert isinstance(created, cc.ProjectInfo)
    assert created.name == "Bepi MSA"
    assert created.role == "owner"
    assert created.personal is False
    listed = {item.id: item.name for item in cc.raw.projects(server.url, server.cookie)}
    assert listed[created.id] == "Bepi MSA"


def test_an_empty_project_name_is_refused(server):
    with pytest.raises(cc.ServerError) as refused:
        cc.raw.create_project(server.url, server.cookie, "")

    assert refused.value.status == 422


def test_members_lists_each_account_with_its_role(server):
    account(server, "viewer@example.org", "viewer")

    members = cc.raw.members(server.url, server.cookie, server.project_id)

    roles = {item["email"]: item["role"] for item in members}
    assert roles == {server.email: "owner", "viewer@example.org": "viewer"}
    assert all(isinstance(item["user_id"], str) for item in members)


def test_a_stranger_cannot_see_the_members(server):
    stranger = account(server, "stranger@example.org")

    with pytest.raises(cc.ServerNotFound) as refused:
        cc.raw.members(server.url, stranger, server.project_id)

    assert refused.value.status == 404


def test_the_owner_sets_a_role(server):
    account(server, "colleague@example.org", "viewer")
    colleague = member(server, "colleague@example.org")

    cc.raw.set_role(server.url, server.cookie, server.project_id, colleague["user_id"], "editor")

    assert member(server, "colleague@example.org")["role"] == "editor"


def test_a_viewer_may_not_set_roles(server):
    viewer = account(server, "viewer@example.org", "viewer")
    account(server, "colleague@example.org", "editor")
    colleague = member(server, "colleague@example.org")

    with pytest.raises(cc.Forbidden) as refused:
        cc.raw.set_role(server.url, viewer, server.project_id, colleague["user_id"], "viewer")

    assert refused.value.status == 403


def test_a_role_the_server_does_not_assign_is_a_schema_error(server):
    account(server, "colleague@example.org", "viewer")
    colleague = member(server, "colleague@example.org")

    with pytest.raises(cc.ServerError) as refused:
        cc.raw.set_role(server.url, server.cookie, server.project_id, colleague["user_id"], "boss")

    assert type(refused.value) is cc.ServerError
    assert refused.value.status == 422


def test_the_owner_removes_a_member(server):
    account(server, "colleague@example.org", "editor")
    colleague = member(server, "colleague@example.org")

    cc.raw.remove_member(server.url, server.cookie, server.project_id, colleague["user_id"])

    emails = [item["email"] for item in cc.raw.members(server.url, server.cookie, server.project_id)]
    assert emails == [server.email]


def test_an_invitation_has_a_link_and_is_listed(server):
    created = cc.raw.invite(
        server.url,
        server.cookie,
        server.project_id,
        "viewer",
        email="guest@example.org",
        expires_in_days=3,
    )

    assert isinstance(created["id"], int)
    assert created["link"] == f"{server.url}/ui/invitation#{created['token']}"
    [listed] = cc.raw.invitations(server.url, server.cookie, server.project_id)
    assert listed["id"] == created["id"]
    assert listed["email"] == "guest@example.org"
    assert listed["role"] == "viewer"
    assert listed["uses"] == 0
    assert listed["max_uses"] == 1
    assert isinstance(listed["expires_at"], str)


def test_an_invitation_without_an_address_is_a_share_link(server):
    created = cc.raw.invite(server.url, server.cookie, server.project_id, "editor", max_uses=5)

    [listed] = cc.raw.invitations(server.url, server.cookie, server.project_id)
    assert listed["id"] == created["id"]
    assert listed["email"] is None
    assert listed["role"] == "editor"
    assert listed["max_uses"] == 5


def test_a_revoked_invitation_is_no_longer_listed(server):
    created = cc.raw.invite(server.url, server.cookie, server.project_id, "editor")

    cc.raw.revoke_invitation(server.url, server.cookie, server.project_id, created["id"])

    assert cc.raw.invitations(server.url, server.cookie, server.project_id) == []


def test_revoking_an_unknown_invitation_is_not_found(server):
    with pytest.raises(cc.ServerNotFound) as refused:
        cc.raw.revoke_invitation(server.url, server.cookie, server.project_id, 4242)

    assert refused.value.status == 404


def test_project_groups_lists_the_granted_groups(server):
    assert cc.raw.project_groups(server.url, server.cookie, server.project_id) == []
    group = call(server, "POST", "/api/groups", {"name": "lab"})
    call(
        server,
        "PUT",
        f"/api/rooms/{server.project_id}/groups/{group['id']}",
        {"role": "viewer"},
    )

    groups = cc.raw.project_groups(server.url, server.cookie, server.project_id)

    assert groups == [{"id": group["id"], "name": "lab", "role": "viewer"}]


def test_a_stranger_cannot_see_the_groups(server):
    stranger = account(server, "stranger@example.org")

    with pytest.raises(cc.ServerNotFound):
        cc.raw.project_groups(server.url, stranger, server.project_id)


def test_project_url_escapes_the_id_as_one_segment():
    url = "https://cocat.example.org/cocat-dev"

    assert (
        cc.raw.project_url(url, "Bepi MSA?#%")
        == "https://cocat.example.org/cocat-dev/ui/rooms/Bepi%20MSA%3F%23%25/browse"
    )
    assert (
        cc.raw.project_url(url + "/", "lab", share=True)
        == "https://cocat.example.org/cocat-dev/ui/rooms/lab/browse#share"
    )


def listed(server, cookie):
    return next(item for item in cc.raw.projects(server.url, cookie) if item.id == server.project_id)


def test_an_editor_invites_while_editors_may_share(server):
    editor = account(server, "editor@example.org", "editor")

    created = cc.raw.invite(server.url, editor, server.project_id, "viewer", email="guest@example.org")

    assert listed(server, editor).editors_can_share is True
    [pending] = cc.raw.invitations(server.url, editor, server.project_id)
    assert pending["id"] == created["id"]


def test_with_sharing_kept_to_the_owner_an_editor_is_forbidden(server):
    editor = account(server, "editor@example.org", "editor")

    cc.raw.set_editors_can_share(server.url, server.cookie, server.project_id, False)

    assert listed(server, editor).editors_can_share is False
    assert listed(server, server.cookie).editors_can_share is False
    with pytest.raises(cc.Forbidden):
        cc.raw.invite(server.url, editor, server.project_id, "viewer")
    with pytest.raises(cc.Forbidden) as refused:
        cc.raw.invitations(server.url, editor, server.project_id)
    assert refused.value.status == 403
    assert cc.raw.invitations(server.url, server.cookie, server.project_id) == []


def test_only_the_owner_changes_who_may_share(server):
    editor = account(server, "editor@example.org", "editor")

    with pytest.raises(cc.Forbidden):
        cc.raw.set_editors_can_share(server.url, editor, server.project_id, False)

    cc.raw.set_editors_can_share(server.url, server.cookie, server.project_id, False)
    cc.raw.set_editors_can_share(server.url, server.cookie, server.project_id, True)
    assert listed(server, editor).editors_can_share is True


def test_a_project_info_lets_editors_share_by_default():
    assert cc.ProjectInfo("a", "A", "owner").editors_can_share is True
    assert cc.ProjectInfo("a", "A", "owner", editors_can_share=False).editors_can_share is False
