import datetime

import numpy as np
import pytest

import cocat_rs as cc

UTC = datetime.timezone.utc
START = datetime.datetime(2025, 1, 31, 10, 0, tzinfo=UTC)
STOP = datetime.datetime(2025, 1, 31, 10, 1, tzinfo=UTC)
PRECISE_START = datetime.datetime(2025, 1, 31, 11, 0, 0, 123456, tzinfo=UTC)
PRECISE_STOP = datetime.datetime(2025, 1, 31, 11, 1, 0, 654321, tzinfo=UTC)


def a_catalogue(room, name="alpha"):
    return room.create_catalogue(
        name=name,
        author="a@x.org",
        tags=["t1", "t2"],
        attributes={"mission": "MMS", "level": 2.0},
    )


def an_event(room, start=START, stop=STOP, rating=3):
    return room.create_event(
        start=start,
        stop=stop,
        author="a@x.org",
        tags=["t1"],
        products=["p1"],
        rating=rating,
        attributes={"q": 1.5, "flag": True},
    )


def test_an_offline_room_is_empty(cache):
    offline = cc.Room.open(str(cache), "aaaaaaaa-1111-4111-8111-111111111111")
    assert offline.status == "offline"
    assert offline.catalogues() == []
    offline.close()


def test_a_catalogue_and_an_event_read_back(room):
    catalogue = a_catalogue(room)
    event = an_event(room)
    catalogue.add_events([event])

    read = room.catalogue("alpha")
    assert read.uuid == catalogue.uuid
    assert read.author == "a@x.org"
    assert read.tags == {"t1", "t2"}
    assert read.attributes == {"mission": "MMS", "level": 2.0}

    read_event = room.event(event.uuid)
    assert read_event.start == START
    assert read_event.stop == STOP
    assert read_event.author == "a@x.org"
    assert read_event.rating == 3
    assert read_event.tags == {"t1"}
    assert read_event.products == {"p1"}
    assert read_event.attributes == {"q": 1.5, "flag": True}
    assert read_event.range == (START, STOP)


def test_times_come_back_as_aware_utc(room):
    catalogue = a_catalogue(room)
    event = an_event(room, start=PRECISE_START, stop=PRECISE_STOP)
    catalogue.add_events([event])

    read = room.event(event.uuid)
    assert read.start == PRECISE_START
    assert read.start.tzinfo is UTC
    assert read.start.microsecond == 123456
    assert read.stop.microsecond == 654321


def test_a_naive_datetime_is_taken_as_utc(room):
    event = room.create_event(
        start=datetime.datetime(2025, 1, 31, 10, 0),
        stop=datetime.datetime(2025, 1, 31, 10, 1),
        author="a@x.org",
    )
    read = room.event(event.uuid)
    assert read.start == START
    assert read.start.tzinfo is UTC


def test_an_unknown_object_raises_runtime_error(room):
    with pytest.raises(RuntimeError):
        room.catalogue("nope")
    with pytest.raises(RuntimeError):
        room.event("bbbbbbbb-2222-4222-8222-222222222222")


def test_invalid_input_raises_value_error(room):
    with pytest.raises(ValueError):
        room.create_event(start="not a time", stop=STOP, author="a@x.org")
    with pytest.raises(ValueError):
        cc.Room.open("/tmp", "../escape")


def test_the_name_and_the_attributes_change(room):
    catalogue = a_catalogue(room)
    catalogue.name = "beta"
    assert room.catalogue(catalogue.uuid).name == "beta"

    catalogue.set_attributes(level=3, platform="MMS4")
    assert catalogue.attributes == {
        "mission": "MMS",
        "level": 3,
        "platform": "MMS4",
    }

    catalogue.remove_attributes(["level", "mission"])
    assert catalogue.attributes == {"platform": "MMS4"}


def test_the_events_of_a_catalogue_change(room):
    catalogue = a_catalogue(room)
    first = an_event(room)
    second = an_event(room, start=PRECISE_START, stop=PRECISE_STOP)

    catalogue.add_events([first, second])
    assert {event.uuid for event in catalogue.events} == {first.uuid, second.uuid}

    catalogue.remove_events([first])
    assert {event.uuid for event in catalogue.events} == {second.uuid}


def test_an_event_range_changes(room):
    event = an_event(room)
    new_start = datetime.datetime(2025, 2, 1, 0, 0, tzinfo=UTC)
    new_stop = datetime.datetime(2025, 2, 1, 0, 1, tzinfo=UTC)

    event.range = (new_start, new_stop)
    assert room.event(event.uuid).range == (new_start, new_stop)

    event.set_attributes(level=4)
    assert room.event(event.uuid).attributes == {"q": 1.5, "flag": True, "level": 4}

    event.remove_attributes(["q"])
    assert room.event(event.uuid).attributes == {"flag": True, "level": 4}


def test_deleting_an_event_removes_it_from_its_catalogue(room):
    catalogue = a_catalogue(room)
    event = an_event(room)
    catalogue.add_events([event])

    event.delete()
    assert catalogue.events == set()
    with pytest.raises(RuntimeError):
        room.event(event.uuid)


def test_deleting_a_catalogue_keeps_its_events(room):
    catalogue = a_catalogue(room)
    event = an_event(room)
    catalogue.add_events([event])

    catalogue.delete()
    assert room.catalogues() == []
    assert room.event(event.uuid).uuid == event.uuid


def test_event_arrays_are_numpy_timestamps_in_time_order(room):
    catalogue = a_catalogue(room)
    later = an_event(room, start=PRECISE_START, stop=PRECISE_STOP)
    earlier = an_event(room)
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


def test_event_arrays_can_carry_the_attributes_from_the_same_read(room):
    """Two reads could straddle a remote change and misalign the lists; one read cannot."""
    catalogue = a_catalogue(room)
    later = an_event(room, start=PRECISE_START, stop=PRECISE_STOP)
    earlier = room.create_event(
        start=START, stop=STOP, author="a@x.org", attributes={"q": 0.5}
    )
    catalogue.add_events([later, earlier])

    arrays = catalogue.event_arrays(attributes=True)

    assert arrays["uuid"] == [earlier.uuid, later.uuid]
    assert arrays["attributes"] == [{"q": 0.5}, {"q": 1.5, "flag": True}]
    assert "attributes" not in catalogue.event_arrays()


def test_room_event_arrays_follow_the_uuid_order_and_skip_unknown(room):
    """One read for a set of events named by uuid, in the caller's order."""
    catalogue = a_catalogue(room)
    later = an_event(room, start=PRECISE_START, stop=PRECISE_STOP)
    earlier = room.create_event(
        start=START, stop=STOP, author="a@x.org", attributes={"q": 0.5}
    )
    catalogue.add_events([later, earlier])
    missing = "bbbbbbbb-2222-4222-8222-222222222222"

    arrays = room.event_arrays(
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
    assert "attributes" not in room.event_arrays([earlier.uuid])


def test_room_event_arrays_of_nothing_are_empty(room):
    arrays = room.event_arrays(
        ["bbbbbbbb-2222-4222-8222-222222222222"], attributes=True
    )
    assert arrays["uuid"] == []
    assert arrays["start"].tolist() == []
    assert arrays["attributes"] == []
