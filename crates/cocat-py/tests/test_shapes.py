"""Shapes and their axis in every export format."""

import pytest

import cocat_rs
from cocat_rs import api
from cocat_rs import tscat as cc_tscat

SHAPE = [
    ("2025-01-31 10:00:00+00:00", 10.0),
    ("2025-01-31 10:00:30.123456+00:00", 0.1 + 0.2),
    ("2025-01-31 10:01:00+00:00", 170.5),
]


def project(tmp_path, name: str):
    return cocat_rs.Project.open(str(tmp_path / name), f"{name}-room")


def pitch_angles(tmp_path):
    """A catalogue of one shaped event and one without, its shapes in pitch angle."""
    source = project(tmp_path, "source")
    shaped = source.create_event(
        start="2025-01-31 10:00:00+00:00", stop="2025-01-31 10:01:00+00:00", shape=SHAPE
    )
    plain = source.create_event(
        start="2025-01-31 11:00:00+00:00", stop="2025-01-31 11:01:00+00:00"
    )
    catalogue = source.create_catalogue(name="pitch", events=[shaped, plain])
    catalogue.shape_axis = ("pitch angle", "deg")
    return source, catalogue, shaped, plain


def assert_same_shapes(target, catalogue, shaped, plain) -> None:
    assert catalogue.shape_axis == ("pitch angle", "deg")
    assert target.event(shaped.uuid).shape == shaped.shape
    assert target.event(plain.uuid).shape is None


def test_a_votable_keeps_shapes_and_their_axis(tmp_path) -> None:
    _, catalogue, shaped, plain = pitch_angles(tmp_path)
    path = tmp_path / "pitch.xml"
    api.export_votable([catalogue], path)

    target = project(tmp_path, "target")
    target.import_votable(str(path))

    assert_same_shapes(target, target.catalogue("pitch"), shaped, plain)


def test_tscat_json_keeps_shapes_and_their_axis(tmp_path) -> None:
    source, catalogue, shaped, plain = pitch_angles(tmp_path)
    path = tmp_path / "pitch.json"
    cc_tscat.export_json(source, [catalogue], path)

    target = project(tmp_path, "target")
    cc_tscat.import_json(target, path)

    assert_same_shapes(target, target.catalogue(catalogue.uuid), shaped, plain)


def test_tfcat_warns_about_an_axis_it_does_not_know(tmp_path) -> None:
    _, catalogue, _, _ = pitch_angles(tmp_path)

    with pytest.warns(UserWarning, match="not strict TFCat"):
        cocat_rs.export_tfcat(catalogue, tmp_path / "pitch.json")

    assert (tmp_path / "pitch.json").exists()
