"""CSV files in and out of a project, the same table the web app reads and writes."""

from datetime import datetime, timezone

import pytest

import cocat_rs

SHEET = """start;end;tags;status;confidence;comment;density
2020-01-01 10:00:00;2020-01-01 10:05:00;shock,foot;reviewed;0.9;sharp jump;12.5
2020-01-02T00:00:00Z;2020-01-02T01:00:00Z;;maybe;;;7
"""


def test_a_csv_file_becomes_a_catalogue(project, tmp_path):
    path = tmp_path / "shocks 2020.csv"
    path.write_text(SHEET)

    with pytest.warns(UserWarning, match="status “maybe”"):
        catalogue = project.import_csv(str(path))

    assert catalogue.name == "shocks 2020"
    first, second = sorted(catalogue.events, key=lambda event: event.start)
    assert first.start == datetime(2020, 1, 1, 10, tzinfo=timezone.utc)
    assert first.tags == {"shock", "foot"}
    assert first.status == "reviewed"
    assert first.confidence == 0.9
    assert first.comment == "sharp jump"
    assert first.attributes == {"density": 12.5}
    assert second.attributes == {"density": 7}


def test_an_exported_csv_imported_again_updates_the_same_events(project, tmp_path):
    source = tmp_path / "shocks.csv"
    source.write_text(SHEET)
    with pytest.warns(UserWarning):
        catalogue = project.import_csv(str(source), name="Shocks")
    exported = tmp_path / "out.csv"

    cocat_rs.export_csv([catalogue], exported)
    exported.write_text(exported.read_text().replace("12.5", "13"))
    again = project.import_csv(str(exported), name="Shocks, edited")

    assert {event.uuid for event in again.events} == {event.uuid for event in catalogue.events}
    edited = max(again.events, key=lambda event: event.attributes["density"])
    assert edited.attributes["density"] == 13


def test_a_csv_without_times_says_so(project, tmp_path):
    path = tmp_path / "bad.csv"
    path.write_text("when,what\n2020,x\n")

    with pytest.raises(ValueError, match="no start column"):
        project.import_csv(str(path))
