"""TFCat import and export."""

import json
from pathlib import Path

import pytest

import cocat_rs

REAL = (
    Path(__file__).parents[2]
    / "cocat-core/tests/fixtures/tfcat/cassini_faraday_patches_2006_excerpt.json"
)


def project(tmp_path, name: str):
    return cocat_rs.Project.open(str(tmp_path / name), f"{name}-room")


def test_a_tfcat_file_imports_as_one_catalogue(tmp_path) -> None:
    imported = project(tmp_path, "target").import_tfcat(str(REAL))

    assert imported.name.startswith("Catalogue of Faraday Rotation (FR) patches")
    assert imported.attributes["instrument_host_name"] == "Cassini"
    events = sorted(imported.events, key=lambda event: event.start)
    assert [event.source_id for event in events] == ["1", "21"]
    assert events[0].attributes["quality"] == 3
    assert str(events[0].start).startswith("2006-09-12")
    assert imported.shape_axis == ("frequency", "Hz")


def test_a_name_given_names_the_catalogue(tmp_path) -> None:
    imported = project(tmp_path, "target").import_tfcat(str(REAL), name="FR patches")
    assert imported.name == "FR patches"


def test_shapes_round_trip_through_a_file(tmp_path) -> None:
    source = project(tmp_path, "source")
    shaped = source.create_event(
        start="2025-01-31 10:00:00+00:00",
        stop="2025-01-31 10:01:00+00:00",
        shape=[
            ("2025-01-31 10:00:00+00:00", 100.0),
            ("2025-01-31 10:01:00+00:00", 100.0),
            ("2025-01-31 10:00:30+00:00", 300.0),
        ],
        source_id="burst-1",
        attributes={"q": 0.5},
    )
    catalogue = source.create_catalogue(name="bursts", events=[shaped])
    path = tmp_path / "bursts.json"
    cocat_rs.export_tfcat(catalogue, path)

    again = project(tmp_path, "target").import_tfcat(str(path))
    [event] = again.events
    assert again.name == "bursts"
    assert event.source_id == "burst-1"
    assert event.attributes == {"q": 0.5}
    assert [value for _, value in event.shape] == [100.0, 100.0, 300.0]
    assert str(event.stop) == "2025-01-31 10:01:00+00:00"


def test_events_without_values_need_a_range(tmp_path) -> None:
    source = project(tmp_path, "source")
    event = source.create_event(
        start="2025-01-31 10:00:00+00:00", stop="2025-01-31 10:01:00+00:00"
    )
    catalogue = source.create_catalogue(name="plain", events=[event])

    with pytest.raises(ValueError, match="give a frequency range"):
        cocat_rs.tfcat_json(catalogue)

    text = cocat_rs.tfcat_json(catalogue, value_range=(1e3, 2e3))
    ring = json.loads(text)["features"][0]["geometry"]["coordinates"][0]
    assert {value for _, value in ring} == {1e3, 2e3}


def test_a_file_without_a_coordinate_system_is_refused(tmp_path) -> None:
    path = tmp_path / "bare.json"
    path.write_text(json.dumps({"type": "FeatureCollection", "features": []}))
    with pytest.raises(ValueError, match="crs"):
        project(tmp_path, "target").import_tfcat(str(path))


def test_a_catalogue_describes_its_shape_axis(tmp_path) -> None:
    catalogue = project(tmp_path, "source").create_catalogue(name="electrons")
    assert catalogue.shape_axis is None

    catalogue.shape_axis = ("energy", "eV")
    assert catalogue.shape_axis == ("energy", "eV")
    crs = json.loads(cocat_rs.tfcat_json(catalogue))["crs"]["properties"]
    assert crs["spectral_coords"] == {"type": "energy", "unit": "eV"}

    catalogue.shape_axis = None
    assert catalogue.shape_axis is None
