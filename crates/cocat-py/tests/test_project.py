import datetime
import warnings

import numpy as np
import pytest

import cocat_rs as cc

UTC = datetime.timezone.utc
START = datetime.datetime(2025, 1, 31, 10, 0, tzinfo=UTC)
STOP = datetime.datetime(2025, 1, 31, 10, 1, tzinfo=UTC)
PRECISE_START = datetime.datetime(2025, 1, 31, 11, 0, 0, 123456, tzinfo=UTC)
PRECISE_STOP = datetime.datetime(2025, 1, 31, 11, 1, 0, 654321, tzinfo=UTC)


def a_catalogue(project, name="alpha"):
    return project.create_catalogue(
        name=name,
        author="a@x.org",
        tags=["t1", "t2"],
        attributes={"mission": "MMS", "level": 2.0},
    )


def an_event(project, start=START, stop=STOP, confidence=0.3):
    return project.create_event(
        start=start,
        stop=stop,
        author="a@x.org",
        tags=["t1"],
        products=["p1"],
        confidence=confidence,
        attributes={"q": 1.5, "flag": True},
    )


def test_an_offline_project_is_empty(cache):
    offline = cc.Project.open(str(cache), "aaaaaaaa-1111-4111-8111-111111111111")
    assert offline.status == "offline"
    assert offline.catalogues() == []
    offline.close()


def test_a_catalogue_and_an_event_read_back(project):
    catalogue = a_catalogue(project)
    event = an_event(project)
    catalogue.add_events([event])

    read = project.catalogue("alpha")
    assert read.uuid == catalogue.uuid
    assert read.author == "a@x.org"
    assert read.tags == {"t1", "t2"}
    assert read.attributes == {"mission": "MMS", "level": 2.0}

    read_event = project.event(event.uuid)
    assert read_event.start == START
    assert read_event.stop == STOP
    assert read_event.author == "a@x.org"
    assert read_event.confidence == 0.3
    assert read_event.status == "confirmed"
    assert read_event.tags == {"t1"}
    assert read_event.products == {"p1"}
    assert read_event.attributes == {"q": 1.5, "flag": True}
    assert read_event.range == (START, STOP)


def test_times_come_back_as_aware_utc(project):
    catalogue = a_catalogue(project)
    event = an_event(project, start=PRECISE_START, stop=PRECISE_STOP)
    catalogue.add_events([event])

    read = project.event(event.uuid)
    assert read.start == PRECISE_START
    assert read.start.tzinfo is UTC
    assert read.start.microsecond == 123456
    assert read.stop.microsecond == 654321


def test_a_naive_datetime_is_taken_as_utc(project):
    event = project.create_event(
        start=datetime.datetime(2025, 1, 31, 10, 0),
        stop=datetime.datetime(2025, 1, 31, 10, 1),
        author="a@x.org",
    )
    read = project.event(event.uuid)
    assert read.start == START
    assert read.start.tzinfo is UTC


def test_an_unknown_object_raises_runtime_error(project):
    with pytest.raises(RuntimeError):
        project.catalogue("nope")
    with pytest.raises(RuntimeError):
        project.event("bbbbbbbb-2222-4222-8222-222222222222")


def test_an_unknown_object_is_not_found(project):
    assert issubclass(cc.NotFound, LookupError)
    assert issubclass(cc.NotFound, RuntimeError)
    with pytest.raises(cc.NotFound) as missing:
        project.catalogue("nope")
    assert isinstance(missing.value, LookupError)
    assert isinstance(missing.value, RuntimeError)
    with pytest.raises(cc.NotFound):
        project.event("bbbbbbbb-2222-4222-8222-222222222222")


def test_invalid_input_raises_value_error(project, tmp_path):
    with pytest.raises(ValueError):
        project.create_event(start="not a time", stop=STOP, author="a@x.org")
    with pytest.raises(ValueError):
        cc.Project.open(tmp_path, "")


def test_a_listed_project_id_opens_whatever_its_characters(tmp_path):
    for project_id in ["Bepi MSA", "../escape", "a/b", ".."]:
        cc.Project.open(tmp_path, project_id).close()
    assert len(list(tmp_path.iterdir())) == 4


def test_the_name_and_the_attributes_change(project):
    catalogue = a_catalogue(project)
    catalogue.name = "beta"
    assert project.catalogue(catalogue.uuid).name == "beta"

    catalogue.set_attributes(level=3, platform="MMS4")
    assert catalogue.attributes == {
        "mission": "MMS",
        "level": 3,
        "platform": "MMS4",
    }

    catalogue.remove_attributes(["level", "mission"])
    assert catalogue.attributes == {"platform": "MMS4"}


def test_the_events_of_a_catalogue_change(project):
    catalogue = a_catalogue(project)
    first = an_event(project)
    second = an_event(project, start=PRECISE_START, stop=PRECISE_STOP)

    catalogue.add_events([first, second])
    assert {event.uuid for event in catalogue.events} == {first.uuid, second.uuid}

    catalogue.remove_events([first])
    assert {event.uuid for event in catalogue.events} == {second.uuid}


def test_an_event_range_changes(project):
    event = an_event(project)
    new_start = datetime.datetime(2025, 2, 1, 0, 0, tzinfo=UTC)
    new_stop = datetime.datetime(2025, 2, 1, 0, 1, tzinfo=UTC)

    event.range = (new_start, new_stop)
    assert project.event(event.uuid).range == (new_start, new_stop)

    event.set_attributes(level=4)
    assert project.event(event.uuid).attributes == {"q": 1.5, "flag": True, "level": 4}

    event.remove_attributes(["q"])
    assert project.event(event.uuid).attributes == {"flag": True, "level": 4}


def test_a_deleted_event_goes_to_the_trash_and_comes_back(project):
    catalogue = a_catalogue(project)
    event = an_event(project)
    catalogue.add_events([event])

    event.delete()
    assert catalogue.events == set()
    assert project.event(event.uuid).status == "deleted"
    assert project.trash() == [event]

    event.restore()
    assert {e.uuid for e in catalogue.events} == {event.uuid}
    assert project.trash() == []


def test_purging_an_event_removes_it_for_good(project):
    catalogue = a_catalogue(project)
    event = an_event(project)
    catalogue.add_events([event])

    event.purge()
    assert catalogue.events == set()
    with pytest.raises(cc.NotFound):
        project.event(event.uuid)


def test_deleting_a_catalogue_keeps_its_events(project):
    catalogue = a_catalogue(project)
    event = an_event(project)
    catalogue.add_events([event])

    catalogue.delete()
    assert project.catalogues() == []
    assert project.catalogue(catalogue.uuid).deleted
    assert project.event(event.uuid).uuid == event.uuid

    catalogue.restore()
    assert project.catalogues() == [catalogue]
    catalogue.purge()
    with pytest.raises(cc.NotFound):
        project.catalogue(catalogue.uuid)


def test_event_arrays_are_numpy_timestamps_in_time_order(project):
    catalogue = a_catalogue(project)
    later = an_event(project, start=PRECISE_START, stop=PRECISE_STOP)
    earlier = an_event(project)
    catalogue.add_events([later, earlier])

    arrays = catalogue.event_arrays()
    assert arrays["uuid"] == [earlier.uuid, later.uuid]
    assert arrays["start"].dtype == np.dtype("datetime64[us]")
    assert arrays["stop"].dtype == np.dtype("datetime64[us]")
    assert arrays["start"].tolist() == [
        np.datetime64("2025-01-31T10:00:00.000000", "us"),
        np.datetime64("2025-01-31T11:00:00.123456", "us"),
    ]

    rows = catalogue.event_attributes(["q"])
    assert rows == [{"q": 1.5}, {"q": 1.5}]
    assert catalogue.event_attributes() == [
        {"q": 1.5, "flag": True},
        {"q": 1.5, "flag": True},
    ]


def test_event_arrays_can_carry_the_attributes_from_the_same_read(project):
    """Two reads could straddle a remote change and misalign the lists; one read cannot."""
    catalogue = a_catalogue(project)
    later = an_event(project, start=PRECISE_START, stop=PRECISE_STOP)
    earlier = project.create_event(
        start=START, stop=STOP, author="a@x.org", attributes={"q": 0.5}
    )
    catalogue.add_events([later, earlier])

    arrays = catalogue.event_arrays(attributes=True)

    assert arrays["uuid"] == [earlier.uuid, later.uuid]
    assert arrays["attributes"] == [{"q": 0.5}, {"q": 1.5, "flag": True}]
    assert "attributes" not in catalogue.event_arrays()


def test_project_event_arrays_follow_the_uuid_order_and_skip_unknown(project):
    """One read for a set of events named by uuid, in the caller's order."""
    catalogue = a_catalogue(project)
    later = an_event(project, start=PRECISE_START, stop=PRECISE_STOP)
    earlier = project.create_event(
        start=START, stop=STOP, author="a@x.org", attributes={"q": 0.5}
    )
    catalogue.add_events([later, earlier])
    missing = "bbbbbbbb-2222-4222-8222-222222222222"

    arrays = project.event_arrays(
        [later.uuid, missing, earlier.uuid], attributes=True
    )

    assert arrays["uuid"] == [later.uuid, earlier.uuid]
    assert arrays["start"].dtype == np.dtype("datetime64[us]")
    assert arrays["start"].tolist() == [
        np.datetime64("2025-01-31T11:00:00.123456", "us"),
        np.datetime64("2025-01-31T10:00:00.000000", "us"),
    ]
    assert arrays["stop"].tolist() == [
        np.datetime64("2025-01-31T11:01:00.654321", "us"),
        np.datetime64("2025-01-31T10:01:00.000000", "us"),
    ]
    assert arrays["attributes"] == [{"q": 1.5, "flag": True}, {"q": 0.5}]
    assert "attributes" not in project.event_arrays([earlier.uuid])


def test_project_event_arrays_of_nothing_are_empty(project):
    arrays = project.event_arrays(
        ["bbbbbbbb-2222-4222-8222-222222222222"], attributes=True
    )
    assert arrays["uuid"] == []
    assert arrays["start"].tolist() == []
    assert arrays["attributes"] == []


def test_an_event_names_the_catalogues_that_list_it(project):
    first = a_catalogue(project, "zeta")
    second = a_catalogue(project, "alpha")
    event = an_event(project)
    unwatched = an_event(project, start=PRECISE_START, stop=PRECISE_STOP)

    first.add_events([event])
    second.add_events([event, unwatched])

    assert event.catalogues == sorted([first.uuid, second.uuid])
    assert project.event(event.uuid).catalogues == sorted([first.uuid, second.uuid])
    assert unwatched.catalogues == [second.uuid]

    first.remove_events([event])
    assert event.catalogues == [second.uuid]


def test_an_event_no_catalogue_lists_has_no_catalogues(project):
    event = an_event(project)
    assert event.catalogues == []


def test_the_v2_fields_read_back_and_change(project):
    shape = [(START, 1e3), (STOP, 2e3)]
    event = project.create_event(
        start=START,
        stop=STOP,
        author="a@x.org",
        status="preliminary",
        confidence=0.5,
        start_err=1.0,
        stop_err=2.0,
        source_id="list-1",
        comment="a long story",
        shape=shape,
    )
    assert event.status == "preliminary"
    assert (event.confidence, event.start_err, event.stop_err) == (0.5, 1.0, 2.0)
    assert event.source_id == "list-1"
    assert event.comment == "a long story"
    assert event.shape == shape
    assert event.origin is None

    event.status = "rejected"
    event.confidence = None
    event.comment = None
    event.shape = None
    assert event.status == "rejected"
    assert event.confidence is None
    assert event.comment is None
    assert event.shape is None

    with pytest.raises(ValueError):
        event.status = "maybe"
    with pytest.raises(ValueError):
        event.confidence = 3.0


def test_a_hand_made_event_records_when_it_was_created(project):
    before = datetime.datetime.now(UTC)
    event = an_event(project)
    assert before <= event.created <= datetime.datetime.now(UTC)
    assert event.created.tzinfo is UTC


def test_a_model_event_names_its_origin(project):
    origin = project.create_origin(
        kind="model",
        name="shock finder",
        version="1.2",
        used=[{"product": "amda/mms1_fgm", "retrieved": "2025-01-01"}],
        parameters={"threshold": 3},
    )
    event = project.create_event(start=START, stop=STOP, origin=origin, confidence=0.9)

    assert event.origin == origin
    assert event.author is None
    assert event.created is None
    read = project.origin(origin)
    assert read["kind"] == "model"
    assert read["name"] == "shock finder"
    assert read["used"] == [{"product": "amda/mms1_fgm", "retrieved": "2025-01-01"}]
    assert read["parameters"] == {"threshold": 3}
    with pytest.raises(cc.NotFound):
        project.origin("nope")


def test_attributes_register_their_type_and_warn_on_a_mismatch(project):
    event = an_event(project)
    assert project.attribute_types()["q"] == {
        "type": "number",
        "unit": None,
        "description": None,
    }

    with pytest.warns(UserWarning, match="registered as number"):
        event.set_attributes(q="high")
    assert event.attributes["q"] == "high"

    project.set_attribute_info("q", unit="nT", description="quality")
    assert project.attribute_types()["q"]["unit"] == "nT"
    with pytest.raises(LookupError, match="type="):
        project.set_attribute_info("nope", unit="s")


def test_an_attribute_is_described_before_any_event_carries_it(project):
    project.set_attribute_info("mlt", unit="h", description="local time", type="number")

    assert project.attribute_types()["mlt"] == {
        "type": "number",
        "unit": "h",
        "description": "local time",
    }
    with pytest.raises(ValueError, match="number, text, bool, date"):
        project.set_attribute_info("x", type="colour")


def test_a_datetime_attribute_is_a_date(project):
    event = an_event(project)
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        event.set_attributes(seen=START)

    assert project.attribute_types()["seen"]["type"] == "date"
    assert event.attributes["seen"] == START


def test_columns_and_existing_tags(project):
    catalogue = a_catalogue(project)
    catalogue.columns = ["q", "flag"]
    assert project.catalogue(catalogue.uuid).columns == ["q", "flag"]

    an_event(project)
    hidden = project.create_event(start=START, stop=STOP, tags=["hidden"])
    hidden.delete()
    assert project.existing_tags() == ["t1", "t2"]


@pytest.mark.parametrize("old", ["Room", "RoomInfo", "rooms"])
def test_the_names_from_before_projects_are_gone(old):
    with pytest.raises(AttributeError):
        getattr(cc, old)


def test_room_id_is_an_unknown_keyword(cache):
    with pytest.raises(TypeError):
        cc.Project.open(cache, room_id="lab")
    with pytest.raises(TypeError):
        cc.raw.history("http://127.0.0.1:1", "cookie", room_id="lab")
