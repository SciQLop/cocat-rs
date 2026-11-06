"""VOTable import and export, and the exchange with Python cocat."""

import json
import os
import subprocess
from datetime import datetime, timezone
from pathlib import Path

import pytest

import cocat_rs
from cocat_rs import api

PYTHON = Path("/home/jeandet/Documents/prog/cocat-python-tests/.venv/bin/python")
CHECKOUT = Path("/home/jeandet/Documents/prog/cocat")
SCRIPT = Path(__file__).parent / "votable_client.py"

ALPHA = "6f9c1a52-0b0d-4f4e-9d5e-1b2c3d4e5f60"
FIRST = "aaaaaaaa-1111-4111-8111-111111111111"
SECOND = "bbbbbbbb-2222-4222-8222-222222222222"


@pytest.fixture(autouse=True)
def wallet(monkeypatch):
    api._reset()
    monkeypatch.setattr(api, "save_on_exit", lambda: None)
    yield
    api._reset()


def python_cocat(mode: str, path: Path, output: Path) -> dict:
    environment = dict(os.environ)
    environment["PYTHONPATH"] = str(CHECKOUT / "src")
    subprocess.run(
        [str(PYTHON), str(SCRIPT), mode, str(path), str(output)],
        check=True,
        capture_output=True,
        env=environment,
    )
    return json.loads(output.read_text())


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
            rating=5,
            attributes={"q": 0.25, "flag": False, "mission": "MMS"},
        ),
        room.create_event(
            start="2025-01-31 11:00:00.123456+00:00",
            stop="2025-01-31 11:01:00.654321+00:00",
            author="rust@x.org",
            uuid=SECOND,
            rating=None,
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
    assert events[0].rating == 5
    assert events[1].rating is None
    assert sorted(events[0].tags) == ["p3"]
    assert sorted(events[0].products) == ["p4"]
    assert events[1].attributes == {"q": 1.5, "flag": True, "mission": "MMS"}


def test_python_cocat_reads_our_export(tmp_path) -> None:
    room = cocat_rs.Room.open(str(tmp_path / "source"), "source-room")
    catalogue_with_events(room)

    path = tmp_path / "from-rust.xml"
    api.export_votable(room.catalogue("gamma"), path)

    read = python_cocat("import", path, tmp_path / "read.json")

    assert [catalogue["name"] for catalogue in read["catalogues"]] == ["gamma"]
    events = {event["uuid"]: event for event in read["events"]}
    first = events[FIRST]
    assert first["start"] == "2025-01-31 10:00:00+00:00"
    assert first["stop"] == "2025-01-31 10:01:00+00:00"
    assert first["rating"] == 5
    assert first["tags"] == ["p3"]
    assert first["products"] == ["p4"]
    assert first["attributes"] == {"flag": False, "mission": "MMS", "q": 0.25}
    second = events[SECOND]
    assert second["start"] == "2025-01-31 11:00:00.123456+00:00"
    assert second["rating"] is None


def test_we_read_python_cocats_export(tmp_path) -> None:
    path = tmp_path / "from-python.xml"
    written = python_cocat("export", path, tmp_path / "written.json")

    room = cocat_rs.Room.open(str(tmp_path / "target"), "target-room")
    count = room.import_votable(str(path))
    assert count == 1

    catalogue = room.catalogue("gamma")
    assert catalogue.author == "python@x.org"
    events = {event.uuid: event for event in catalogue.events}
    assert set(events) == {FIRST, SECOND}
    for expected in written["events"]:
        event = events[expected["uuid"]]
        # Python cocat exported a naive time; the document stores it as UTC, and this
        # client always hands Python an aware datetime.
        assert event.start == datetime.fromisoformat(expected["start"]).replace(
            tzinfo=timezone.utc
        )
        assert event.stop == datetime.fromisoformat(expected["stop"]).replace(
            tzinfo=timezone.utc
        )
        assert event.author == expected["author"]
        assert event.rating == expected["rating"]
        assert sorted(event.tags) == expected["tags"]
        assert sorted(event.products) == expected["products"]
        assert event.attributes == expected["attributes"]
