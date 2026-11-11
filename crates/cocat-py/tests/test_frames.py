"""Catalogues as pandas and polars DataFrames, and DataFrames as catalogues."""

from datetime import datetime, timezone

import pytest

pd = pytest.importorskip("pandas")
pl = pytest.importorskip("polars")

START = datetime(2020, 1, 1, tzinfo=timezone.utc)
STOP = datetime(2020, 1, 1, 0, 10, tzinfo=timezone.utc)
FIELDS = ["uuid", "start", "stop", "tags", "status", "confidence", "author", "comment"]


def shocks(project):
    first = project.create_event(
        START, STOP, tags=["shock"], confidence=0.8, comment="clear", attributes={"mach": 3.2}
    )
    second = project.create_event(
        datetime(2020, 1, 2, tzinfo=timezone.utc),
        datetime(2020, 1, 2, 1, tzinfo=timezone.utc),
        attributes={"mach": 2.5, "spacecraft": "MMS1"},
    )
    return project.create_catalogue(name="shocks", events=[second, first]), first, second


def test_a_catalogue_as_pandas_has_one_row_per_event_in_time_order(project):
    catalogue, first, second = shocks(project)

    frame = catalogue.to_pandas()

    assert list(frame.columns) == [*FIELDS, "mach", "spacecraft"]
    assert list(frame["uuid"]) == [first.uuid, second.uuid]
    assert frame["start"].iloc[0] == pd.Timestamp(START)
    assert frame["tags"].iloc[0] == ["shock"]
    assert frame["comment"].iloc[0] == "clear"
    assert list(frame["mach"]) == [3.2, 2.5]
    assert pd.isna(frame["spacecraft"].iloc[0])


def test_a_catalogue_as_polars_has_the_same_columns(project):
    catalogue, first, _ = shocks(project)

    frame = catalogue.to_polars()

    assert frame.columns == [*FIELDS, "mach", "spacecraft"]
    assert frame["uuid"][0] == first.uuid
    assert frame["mach"].to_list() == [3.2, 2.5]


def test_an_empty_catalogue_still_has_its_columns(project):
    empty = project.create_catalogue(name="empty")

    assert list(empty.to_pandas().columns) == FIELDS
    assert empty.to_polars().columns == FIELDS


def test_a_pandas_frame_becomes_a_catalogue(project):
    frame = pd.DataFrame(
        {
            "start": pd.to_datetime(["2021-03-01 10:00", "2021-03-02 10:00"]),
            "stop": pd.to_datetime(["2021-03-01 10:30", "2021-03-02 10:30"]),
            "tags": [["cme", "halo"], "cme; partial"],
            "confidence": [0.5, float("nan")],
            "speed": [1200, 450],
            "note": ["fast", None],
        }
    )

    catalogue = project.import_dataframe(frame, name="CMEs")

    assert catalogue.name == "CMEs"
    first, second = sorted(catalogue.events, key=lambda event: event.start)
    assert first.start == datetime(2021, 3, 1, 10, tzinfo=timezone.utc)
    assert first.tags == {"cme", "halo"}
    assert second.tags == {"cme", "partial"}
    assert first.confidence == 0.5
    assert second.confidence is None
    assert first.attributes == {"speed": 1200, "note": "fast"}
    assert second.attributes == {"speed": 450}


def test_a_polars_frame_becomes_a_catalogue(project):
    frame = pl.DataFrame(
        {"start": [START], "stop": [STOP], "status": ["final"], "kind": ["bow"]}
    )

    catalogue = project.import_dataframe(frame, name="from polars")

    (event,) = catalogue.events
    assert event.range == (START, STOP)
    assert event.status == "final"
    assert event.attributes == {"kind": "bow"}


def test_a_frame_edited_and_imported_again_updates_its_events(project):
    catalogue, first, _ = shocks(project)
    frame = catalogue.to_pandas()
    frame.loc[0, "mach"] = 4.5
    frame.loc[0, "stop"] = pd.Timestamp("2020-01-01 00:20", tz="UTC")

    again = project.import_dataframe(frame, name="shocks, edited")

    assert {event.uuid for event in again.events} == {event.uuid for event in catalogue.events}
    assert project.event(first.uuid).attributes["mach"] == 4.5
    assert project.event(first.uuid).stop == datetime(2020, 1, 1, 0, 20, tzinfo=timezone.utc)
    assert project.event(first.uuid).comment == "clear"


def test_a_frame_without_times_says_so(project):
    with pytest.raises(ValueError, match="no stop column"):
        project.import_dataframe(pd.DataFrame({"start": [START]}), name="x")
