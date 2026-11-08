"""The names from before rooms were called projects still work, and say what replaced them."""

import pytest

import cocat_rs as cc
from cocat_rs import api
from cocat_rs import tscat as cc_tscat

UNREACHABLE = "http://127.0.0.1:1"


@pytest.mark.parametrize(
    ("old", "new"),
    [("Room", "Project"), ("RoomInfo", "ProjectInfo"), ("rooms", "projects")],
)
def test_an_old_name_is_the_new_object_and_warns(old, new):
    with pytest.warns(DeprecationWarning, match=f"cocat_rs.{old}.* is now cocat_rs.{new}"):
        value = getattr(cc, old)
    assert value is getattr(cc, new)


def test_the_old_listing_names_the_new_function():
    with pytest.warns(
        DeprecationWarning, match=r"cocat_rs\.rooms\(\) is now cocat_rs\.projects\(\)"
    ):
        cc.rooms  # noqa: B018


def test_old_names_stay_out_of_star_imports():
    assert not {"Room", "RoomInfo", "rooms"} & set(cc.__all__)


def test_an_unknown_name_is_still_an_attribute_error():
    with pytest.raises(AttributeError):
        cc.Nothing  # noqa: B018


def test_a_project_is_a_room(cache):
    project = cc.Project.open(cache, "lab")
    with pytest.warns(DeprecationWarning):
        assert isinstance(project, cc.Room)


def test_the_old_listing_still_lists(server):
    with pytest.warns(DeprecationWarning):
        listed = cc.rooms(server.url, server.cookie)
    assert server.project_id in [info.id for info in listed]


def test_room_id_still_opens_a_project(cache):
    with pytest.warns(DeprecationWarning, match="room_id= is now project_id="):
        project = cc.Project.open(cache, room_id="lab")
    assert project.catalogues() == []


@pytest.mark.parametrize(
    ("function", "arguments"),
    [
        (cc.history, {}),
        (cc.restore, {"version": 1}),
        (cc.name_version, {"version": 1, "name": "v"}),
        (cc.commit, {}),
        (cc.copy_version, {"version": 1, "name": "v"}),
        (cc.Project.at, {"version": 1}),
    ],
)
def test_room_id_is_still_accepted_by_the_server_calls(function, arguments):
    with (
        pytest.warns(DeprecationWarning, match="room_id= is now project_id="),
        pytest.raises(ValueError),
    ):
        function(UNREACHABLE, "cookie", room_id="lab", **arguments)


def test_room_id_still_configures_the_script_mode():
    api._reset()
    try:
        with pytest.warns(DeprecationWarning, match="room_id= is now project_id="):
            api.set_config(room_id="lab")
        assert api.SESSION.project_id == "lab"
    finally:
        api._reset()


def test_room_is_still_accepted_by_the_tscat_functions(cache, tmp_path):
    project = cc.Project.open(cache, "lab")
    path = tmp_path / "tscat.json"
    path.write_text('{"events": [], "catalogues": []}')
    with pytest.warns(DeprecationWarning, match="room= is now project="):
        assert cc_tscat.import_json(room=project, path=path) == []
