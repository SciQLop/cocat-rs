"""The release number is written once, in the workspace Cargo.toml; the package takes it."""

import importlib.metadata
import pathlib
import tomllib

import cocat_rs

ROOT = pathlib.Path(__file__).resolve().parents[3]


def release_version():
    with open(ROOT / "Cargo.toml", "rb") as manifest:
        return tomllib.load(manifest)["workspace"]["package"]["version"]


def test_the_pyproject_takes_its_version_from_cargo():
    with open(ROOT / "crates" / "cocat-py" / "pyproject.toml", "rb") as manifest:
        project = tomllib.load(manifest)["project"]
    assert "version" not in project, "no second place to bump"
    assert "version" in project["dynamic"]


def test_the_installed_package_carries_the_release_version():
    assert release_version() != "0.0.0"
    assert importlib.metadata.version("cocat-rs") == release_version()
    assert cocat_rs.__version__ == release_version()
