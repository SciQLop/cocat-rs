"""VOTable import and export."""

import pytest

import cocat_rs
from cocat_rs import api

ALPHA = "6f9c1a52-0b0d-4f4e-9d5e-1b2c3d4e5f60"
FIRST = "aaaaaaaa-1111-4111-8111-111111111111"
SECOND = "bbbbbbbb-2222-4222-8222-222222222222"


@pytest.fixture(autouse=True)
def wallet(monkeypatch):
    api._reset()
    monkeypatch.setattr(api, "save_on_exit", lambda: None)
    yield
    api._reset()


def catalogue_with_events(room) -> None:
    catalogue = room.create_catalogue(
        name="gamma",
        author="rust@x.org",
        uuid=ALPHA,
        tags=["t1"],
        attributes={"mission": "MMS"},
    )
    events = [
        room.create_event(
            start="2025-01-31 10:00:00+00:00",
            stop="2025-01-31 10:01:00+00:00",
            author="rust@x.org",
            uuid=FIRST,
            tags=["p3"],
            products=["p4"],
            confidence=0.5,
            status="reviewed",
            start_err=1.5,
            source_id="list-1",
            attributes={"q": 0.25, "flag": False, "mission": "MMS"},
        ),
        room.create_event(
            start="2025-01-31 11:00:00.123456+00:00",
            stop="2025-01-31 11:01:00.654321+00:00",
            author="rust@x.org",
            uuid=SECOND,
            attributes={"q": 1.5, "flag": True, "mission": "MMS"},
        ),
    ]
    catalogue.add_events(events)


def test_a_votable_round_trips_through_our_own_import(tmp_path) -> None:
    source = cocat_rs.Room.open(str(tmp_path / "source"), "source-room")
    catalogue_with_events(source)

    path = tmp_path / "gamma.xml"
    api.export_votable([source.catalogue("gamma")], path)

    target = cocat_rs.Room.open(str(tmp_path / "target"), "target-room")
    target.import_votable(str(path))

    imported = target.catalogue("gamma")
    assert imported.author == "rust@x.org"
    events = sorted(imported.events, key=lambda event: event.start)
    assert [event.uuid for event in events] == [FIRST, SECOND]
    assert str(events[0].start) == "2025-01-31 10:00:00+00:00"
    assert str(events[1].start) == "2025-01-31 11:00:00.123456+00:00"
    assert events[0].confidence == 0.5
    assert events[0].status == "reviewed"
    assert events[0].start_err == 1.5
    assert events[0].source_id == "list-1"
    assert events[1].confidence is None
    assert events[1].status == "confirmed"
    assert events[1].created == source.event(SECOND).created
    assert sorted(events[0].tags) == ["p3"]
    assert sorted(events[0].products) == ["p4"]
    assert events[1].attributes == {"q": 1.5, "flag": True, "mission": "MMS"}
