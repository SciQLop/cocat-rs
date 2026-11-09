"""Merging a project that never connected into another, like local catalogues into the
personal project at the first sign-in."""

import datetime
import time

import pytest
from test_read_only import READ_ONLY, connected, shared, viewer_cache, viewer_cookie

import cocat_rs as cc

UTC = datetime.timezone.utc
START = datetime.datetime(2025, 1, 31, 10, 0, tzinfo=UTC)
STOP = datetime.datetime(2025, 1, 31, 10, 1, tzinfo=UTC)
LOCAL_ID = "11111111-1111-4111-8111-111111111111"


@pytest.fixture
def local(tmp_path):
    directory = tmp_path / "local"
    directory.mkdir()
    project = cc.Project.open(str(directory), LOCAL_ID)
    try:
        yield project
    finally:
        project.close()


def fill(local):
    origin = local.create_origin("detector", "finder", version="1.0")
    kept = local.create_event(
        start=START, stop=STOP, attributes={"b": 2.5}, origin=origin, comment="seen"
    )
    trashed = local.create_event(start=START, stop=STOP)
    trashed.delete()
    catalogue = local.create_catalogue(name="waves", tags=["t"], events=[kept])
    catalogue.shape_axis = ("frequency", "kHz")
    local.set_attribute_info("b", unit="nT", description="field")
    smart = local.create_smart_catalogue("strong", "b > 2")
    return catalogue, kept, trashed, smart, origin


def wait_until(predicate, timeout=15.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.02)
    raise AssertionError("the condition did not hold in time")


def test_a_merge_brings_everything_with_its_uuids(project, local):
    catalogue, kept, trashed, smart, origin = fill(local)

    assert project.merge(local) == {"catalogues": 2, "events": 2}

    merged = project.catalogue(catalogue.uuid)
    assert merged.name == "waves"
    assert merged.tags == {"t"}
    assert merged.shape_axis == ("frequency", "kHz")
    assert {event.uuid for event in merged.events} == {kept.uuid}
    assert project.catalogue(smart.uuid).query == "b > 2"
    assert [event.uuid for event in project.trash()] == [trashed.uuid]
    event = project.event(kept.uuid)
    assert event.comment == "seen"
    assert event.origin == origin
    assert project.origin(origin)["name"] == "finder"
    assert project.attribute_types()["b"] == {
        "type": "number",
        "unit": "nT",
        "description": "field",
    }


def test_the_source_is_left_as_it_is(project, local):
    fill(local)
    project.create_catalogue(name="already there")

    project.merge(local)

    assert {c.name for c in local.catalogues()} == {"waves", "strong"}
    assert local.status == "offline"


def test_merging_twice_adds_nothing_and_later_edits_come_alone(project, local):
    catalogue, *_ = fill(local)
    project.merge(local)

    assert project.merge(local) == {"catalogues": 0, "events": 0}

    local.create_event(start=START, stop=STOP)
    catalogue.name = "renamed"
    assert project.merge(local) == {"catalogues": 0, "events": 1}
    assert project.catalogue(catalogue.uuid).name == "renamed"
    assert len(project.catalogues()) == 2


def test_the_merge_reaches_the_server(project, other, local):
    catalogue, *_ = fill(local)

    project.merge(local)
    project.save(15)

    wait_until(lambda: {c.uuid for c in other.catalogues()} >= {catalogue.uuid})
    assert other.attribute_types()["b"]["unit"] == "nT"


def test_both_registries_stay_readable(project, local):
    project.create_event(start=START, stop=STOP, attributes={"b": 1.0, "only_here": "x"})
    project.set_attribute_info("b", unit="T")
    fill(local)

    project.merge(local)

    types = project.attribute_types()
    assert types["b"]["type"] == "number"
    assert types["b"]["unit"] in {"T", "nT"}
    assert types["only_here"]["type"] == "text"
    assert len(project.catalogues()) == 2


def test_a_read_only_target_refuses_the_merge(server, shared, viewer_cache, local):
    viewer = connected(server, viewer_cache, viewer_cookie(server))
    wait_until(lambda: viewer.role == "viewer")
    fill(local)

    with pytest.raises(RuntimeError, match=READ_ONLY):
        viewer.merge(local)
    assert [c.name for c in viewer.catalogues()] == ["shared"]
    viewer.close()
